use super::super::*;
use super::common::{cleanup_dir, test_manager};
use color_eyre::eyre::Result;
use kraai_types::{ExecutionPolicy, MessageStatus};
use std::path::PathBuf;

#[tokio::test]
async fn create_session_returns_usable_session_id() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let sessions = manager.list_sessions().await?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);
    assert_eq!(
        sessions[0].selected_profile_id.as_deref(),
        Some("plan-code")
    );
    assert_eq!(manager.get_tip(&session_id).await?, None);

    cleanup_dir(data_dir).await;
    Ok(())
}

#[test]
fn title_from_user_prompt_truncates_to_sixty_characters() {
    let prompt = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let title = title_from_user_prompt(prompt).expect("title should be present");

    assert_eq!(
        title,
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567"
    );
    assert_eq!(title.chars().count(), 60);
}

#[test]
fn title_from_user_prompt_flattens_newlines() {
    let title =
        title_from_user_prompt("first line\nsecond\r\nthird").expect("title should be present");

    assert_eq!(title, "first line second third");
    assert!(!title.contains('\n'));
    assert!(!title.contains('\r'));
}

#[tokio::test]
async fn last_used_profile_is_inherited_by_new_sessions() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let first_session = manager.create_session().await?;
    manager
        .set_session_profile(&first_session, String::from("plan-code"))
        .await?;
    let _request = manager
        .prepare_start_stream(
            &first_session,
            String::from("hello"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;

    let second_session = manager.create_session().await?;
    let sessions = manager.list_sessions().await?;
    let inherited = sessions
        .into_iter()
        .find(|session| session.id == second_session)
        .unwrap();
    assert_eq!(inherited.selected_profile_id.as_deref(), Some("plan-code"));

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn profile_changes_are_rejected_while_turn_is_active() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
        .await?;
    let _request = manager
        .prepare_start_stream(
            &session_id,
            String::from("hello"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;

    let locked = manager
        .set_session_profile(&session_id, String::from("build-code"))
        .await;
    assert!(locked.is_err());

    manager.clear_active_turn(&session_id);
    manager
        .set_session_profile(&session_id, String::from("build-code"))
        .await?;

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn sessions_keep_independent_tips_and_histories() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_a = manager.create_session().await?;
    let session_b = manager.create_session().await?;

    let a_message = manager
        .add_message(&session_a, ChatRole::User, String::from("hello a"), None)
        .await?;
    let b_message = manager
        .add_message(&session_b, ChatRole::User, String::from("hello b"), None)
        .await?;

    assert_eq!(manager.get_tip(&session_a).await?, Some(a_message.clone()));
    assert_eq!(manager.get_tip(&session_b).await?, Some(b_message.clone()));

    let history_a = manager.get_chat_history(&session_a).await?;
    let history_b = manager.get_chat_history(&session_b).await?;

    assert_eq!(history_a.len(), 1);
    assert_eq!(history_b.len(), 1);
    assert_eq!(history_a.get(&a_message).unwrap().content, "hello a");
    assert_eq!(history_b.get(&b_message).unwrap().content, "hello b");

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn first_user_message_sets_session_title() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"),
            None,
        )
        .await?;

    let session = manager.require_session(&session_id).await?;
    assert_eq!(
        session.title.as_deref(),
        Some("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ01234567")
    );

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn later_user_messages_do_not_overwrite_session_title() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("first prompt"),
            None,
        )
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::Assistant,
            String::from("assistant response"),
            None,
        )
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("second prompt should not replace the title"),
            None,
        )
        .await?;

    let session = manager.require_session(&session_id).await?;
    assert_eq!(session.title.as_deref(), Some("first prompt"));

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn deleting_session_aborts_stream_and_removes_transient_state() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let stable_tip = manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("before stream"),
            None,
        )
        .await?;
    let streaming_id = manager
        .start_streaming_message(
            &session_id,
            ChatRole::Assistant,
            CallId::new("call-1"),
            None,
            None,
        )
        .await?;

    assert_eq!(
        manager.get_tip(&session_id).await?,
        Some(streaming_id.clone())
    );

    manager.delete_session(&session_id).await?;

    assert!(manager.get_tip(&session_id).await?.is_none());
    assert!(manager.get_chat_history(&session_id).await?.is_empty());
    assert!(
        manager
            .streaming_messages
            .read()
            .await
            .get(&streaming_id)
            .is_none()
    );
    assert!(!manager.message_store.exists(&stable_tip).await?);

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn workspace_and_pending_tools_are_isolated_per_session() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_a = manager.create_session().await?;
    let session_b = manager.create_session().await?;

    manager
        .set_workspace_dir(&session_a, PathBuf::from("/tmp/workspace-a"))
        .await?;

    let call_id = CallId::new("call-a");
    manager
        .session_states
        .get_mut(&session_a)
        .unwrap()
        .pending_tool_calls
        .insert(
            call_id.clone(),
            PendingToolCall {
                call: ToolCall {
                    call_id: call_id.clone(),
                    tool_id: ToolId::new("list_files"),
                    args: serde_json::json!({ "path": "." }),
                },
                source_message_id: MessageId::new("msg-a"),
                prepared: manager
                    .tools
                    .prepare_tool(
                        &ToolId::new("list_files"),
                        serde_json::json!({ "path": "." }),
                    )
                    .expect("prepare list_files tool"),
                description: String::from("test"),
                assessment: ToolCallAssessment {
                    risk: RiskLevel::ReadOnlyWorkspace,
                    policy: ExecutionPolicy::AlwaysAsk,
                    reasons: Vec::new(),
                },
                config: kraai_types::ToolCallGlobalConfig {
                    workspace_dir: PathBuf::from("/tmp/workspace-a"),
                },
                tool_state_snapshot: ToolStateSnapshot::default(),
                status: PermissionStatus::Pending,
                queue_order: 0,
            },
        );

    let workspace_a = manager.get_workspace_dir_state(&session_a).await?.unwrap();
    let workspace_b = manager.get_workspace_dir_state(&session_b).await?.unwrap();

    assert_eq!(workspace_a.0, PathBuf::from("/tmp/workspace-a"));
    assert!(workspace_a.1);
    assert_eq!(workspace_b.0, PathBuf::from("/tmp/default-workspace"));
    assert!(!workspace_b.1);
    assert!(manager.has_pending_tools(&session_a));
    assert!(!manager.has_pending_tools(&session_b));
    assert!(!manager.approve_tool(&session_b, call_id.clone()));
    assert!(manager.approve_tool(&session_a, call_id));

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn new_sessions_default_to_plan_code_on_fresh_manager() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let session = manager
        .list_sessions()
        .await?
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();

    assert_eq!(session.selected_profile_id.as_deref(), Some("plan-code"));

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn new_sessions_inherit_last_used_profile_after_turn_starts() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let first_session = manager.create_session().await?;
    manager
        .set_session_profile(&first_session, String::from("build-code"))
        .await?;
    let pending = manager
        .prepare_start_stream(
            &first_session,
            String::from("build something"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.abort_streaming_message(&pending.message_id).await?;
    manager.clear_active_turn(&first_session);

    let second_session = manager.create_session().await?;
    let inherited = manager
        .list_sessions()
        .await?
        .into_iter()
        .find(|session| session.id == second_session)
        .unwrap();

    assert_eq!(inherited.selected_profile_id.as_deref(), Some("build-code"));

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn prepare_start_stream_fails_when_no_profile_is_selected() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let mut session = manager
        .session_store
        .get(&session_id)
        .await?
        .expect("session should exist");
    session.selected_profile_id = None;
    manager.session_store.save(&session).await?;
    let error = manager
        .prepare_start_stream(
            &session_id,
            String::from("hello"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("No profile selected"));

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn undo_last_user_message_rewinds_tip_and_returns_message_content() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let first_user = manager
        .add_message(&session_id, ChatRole::User, String::from("first"), None)
        .await?;
    let second_user = manager
        .add_message(&session_id, ChatRole::User, String::from("second"), None)
        .await?;
    let assistant = manager
        .add_message(
            &session_id,
            ChatRole::Assistant,
            String::from("reply"),
            None,
        )
        .await?;

    assert_eq!(manager.get_tip(&session_id).await?, Some(assistant));

    let restored = manager.undo_last_user_message(&session_id).await?;

    assert_eq!(restored.as_deref(), Some("second"));
    assert_eq!(
        manager.get_tip(&session_id).await?,
        Some(first_user.clone())
    );

    let history = manager.get_chat_history(&session_id).await?;
    assert!(history.contains_key(&first_user));
    assert!(!history.contains_key(&second_user));
    assert_eq!(history.len(), 1);

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn start_stream_failure_rolls_tip_back_to_last_durable_message() -> Result<()> {
    let data_dir = super::common::test_dir("stream-failure");
    tokio::fs::create_dir_all(&data_dir).await.unwrap();

    let message_store = Arc::new(kraai_persistence::FileMessageStore::new(&data_dir));
    let session_store = Arc::new(kraai_persistence::FileSessionStore::new(
        &data_dir,
        message_store.clone(),
    ));
    let manager_providers = ProviderManager::new();
    let mut tools = ToolManager::new();
    tools.register_tool(super::common::MockTool { name: "close_file" });
    tools.register_tool(super::common::MockTool { name: "list_files" });
    tools.register_tool(super::common::MockTool { name: "open_file" });
    tools.register_tool(super::common::MockTool {
        name: "search_files",
    });
    tools.register_tool(super::common::MockTool { name: "read_files" });
    tools.register_tool(super::common::MockTool { name: "edit_file" });
    let mut manager = AgentManager::new(
        manager_providers,
        tools,
        PathBuf::from("/tmp/default-workspace"),
        message_store,
        session_store,
    );

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
        .await?;
    manager
        .add_message(&session_id, ChatRole::User, String::from("hello"), None)
        .await?;

    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("trigger failure"),
            ModelId::new("mock-model"),
            ProviderId::new("missing-provider"),
        )
        .await?;
    let result = manager
        .cloned_provider_manager()
        .generate_reply_stream(
            request.provider_id,
            &request.model_id,
            request.provider_messages,
            kraai_provider_core::ProviderRequestContext::default(),
        )
        .await;
    assert!(result.is_err());
    manager.abort_streaming_message(&request.message_id).await?;

    let tip = manager.get_tip(&session_id).await?;
    let history = manager.get_chat_history(&session_id).await?;
    let latest_user_message = history
        .values()
        .find(|message| message.role == ChatRole::User && message.content == "trigger failure")
        .unwrap();

    assert_eq!(tip, Some(latest_user_message.id.clone()));
    assert_eq!(history.len(), 2);
    assert!(
        history
            .values()
            .all(|message| message.status == MessageStatus::Complete)
    );

    cleanup_dir(data_dir).await;
    Ok(())
}
