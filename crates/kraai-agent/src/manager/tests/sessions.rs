use super::super::*;
use super::common::{cleanup_dir, test_manager};
use color_eyre::eyre::Result;
use kraai_types::MessageStatus;
use std::path::PathBuf;

#[tokio::test]
async fn a_foreign_workspace_change_refreshes_cached_state_before_the_next_turn() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    assert!(manager.prepare_session(&session_id).await?);
    let obsolete = data_dir.join("obsolete-workspace");
    manager.set_workspace_dir(&session_id, obsolete).await?;
    let foreign = kraai_persistence::Persistence::open(&data_dir).await?;
    let workspace = data_dir.join("foreign-workspace");
    tokio::fs::create_dir_all(&workspace).await?;
    let mut session = foreign.sessions().get(&session_id).await?.unwrap();
    session.workspace_dir = workspace.clone();
    foreign.sessions().save(&session).await?;
    assert_eq!(
        manager.get_workspace_dir_state(&session_id).await?,
        Some((workspace.clone(), false))
    );
    manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    assert_eq!(
        manager.script_turn_context(&session_id)?.workspace_dir,
        workspace
    );
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn undo_returns_input_after_the_tip_commit_even_if_cleanup_fails() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            "restore this input".into(),
            None,
        )
        .await?;
    let connection = rusqlite::Connection::open(data_dir.join("kraai.sqlite3"))?;
    connection.execute_batch("CREATE TRIGGER fail_cleanup BEFORE DELETE ON records WHEN OLD.kind = 'message' BEGIN SELECT RAISE(FAIL, 'injected cleanup failure'); END;")?;
    assert_eq!(
        manager.undo_last_user_message(&session_id).await?,
        Some("restore this input".into())
    );
    assert!(manager.get_tip(&session_id).await?.is_none());
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn preparing_session_rejects_a_parent_cycle() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let message_id = manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("cycle").into(),
            None,
        )
        .await?;
    let mut message = manager.message_store.get(&message_id).await?.unwrap();
    message.parent_id = Some(message_id.clone());
    manager.message_store.save(&message).await?;

    let error = manager.prepare_session(&session_id).await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains(&format!("cycle repeats message {message_id}"))
    );
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn restored_session_supplies_script_workspace_independently_of_executable() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace = data_dir.join("configured-workspace");
    tokio::fs::create_dir_all(&workspace).await?;
    let session_id = manager
        .create_session_with(Some(workspace.clone()), None)
        .await?;
    manager.session_states.clear();
    assert!(manager.prepare_session(&session_id).await?);
    manager
        .prepare_start_stream(
            &session_id,
            String::from("probe").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    let context = manager.script_turn_context(&session_id)?;
    assert_eq!(context.workspace_dir, workspace);
    assert!(!std::env::current_exe()?.starts_with(&context.workspace_dir));
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn create_session_returns_usable_session_id() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let sessions = manager.list_sessions().await?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);
    assert_eq!(sessions[0].selected_profile_id.as_deref(), Some("coding"));
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

#[tokio::test]
async fn session_defaults_follow_profiles_available_in_the_target_workspace() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_a = data_dir.join("workspace-a");
    let workspace_b = data_dir.join("workspace-b");
    tokio::fs::create_dir_all(workspace_a.join(".kraai")).await?;
    tokio::fs::create_dir_all(&workspace_b).await?;
    tokio::fs::write(
        workspace_a.join(".kraai/agents.toml"),
        r#"[[profiles]]
id = "workspace-only"
extends = "coding"
display_name = "Workspace only"
description = "Local profile"
system_prompt = "Local instructions"
commands = []
capabilities = []
nushell_startup = "clean"
"#,
    )
    .await?;
    manager
        .create_session_with(Some(workspace_a.clone()), Some("workspace-only".into()))
        .await?;

    let (_, catalog_a) = manager.list_agent_profiles_for_workspace(Some(&workspace_a));
    assert_eq!(
        catalog_a.selected_profile_id.as_deref(),
        Some("workspace-only")
    );
    let session_a = manager.create_session_with(Some(workspace_a), None).await?;
    assert_eq!(
        manager
            .require_session(&session_a)
            .await?
            .selected_profile_id
            .as_deref(),
        Some("workspace-only")
    );

    let (_, catalog_b) = manager.list_agent_profiles_for_workspace(Some(&workspace_b));
    assert_eq!(catalog_b.selected_profile_id.as_deref(), Some("coding"));
    let session_b = manager
        .create_session_with(Some(workspace_b.clone()), None)
        .await?;
    assert_eq!(
        manager
            .require_session(&session_b)
            .await?
            .selected_profile_id,
        catalog_b.selected_profile_id
    );

    let count = manager.list_sessions().await?.len();
    let error = manager
        .create_session_with(Some(workspace_b), Some("workspace-only".into()))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Unknown profile: workspace-only")
    );
    assert_eq!(manager.list_sessions().await?.len(), count);
    cleanup_dir(data_dir).await;
    Ok(())
}

#[test]
fn title_from_user_prompt_flattens_newlines() {
    let title =
        title_from_user_prompt("first line\nsecond\r\nthird").expect("title should be present");

    assert_eq!(title, "first line second third");
    assert!(!title.contains('\n'));
    assert!(!title.contains('\r'));
}

#[test]
fn title_from_user_prompt_preserves_unicode_whitespace_and_truncation_boundaries() {
    for (prompt, expected) in [
        (String::new(), None),
        (String::from(" \t\r\n\u{2003}\u{a0}"), None),
        (
            String::from("\t é \u{2003}模型\u{a0}🦀\n"),
            Some(String::from("é 模型 🦀")),
        ),
        (
            format!("{} next", "é".repeat(59)),
            Some(format!("{} ", "é".repeat(59))),
        ),
        (format!("{}\nnext", "🦀".repeat(60)), Some("🦀".repeat(60))),
        ("word \t".repeat(100_000), Some("word ".repeat(12))),
    ] {
        assert_eq!(title_from_user_prompt(&prompt), expected);
    }
}

#[tokio::test]
async fn profile_changes_are_rejected_while_turn_is_active() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("coding-no-sandbox"))
        .await?;
    let _request = manager
        .prepare_start_stream(
            &session_id,
            String::from("hello").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;

    let locked = manager
        .set_session_profile(&session_id, String::from("coding"))
        .await;
    assert!(locked.is_err());

    manager.clear_active_turn(&session_id);
    manager
        .set_session_profile(&session_id, String::from("coding"))
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
        .add_message(
            &session_a,
            ChatRole::User,
            String::from("hello a").into(),
            None,
        )
        .await?;
    let b_message = manager
        .add_message(
            &session_b,
            ChatRole::User,
            String::from("hello b").into(),
            None,
        )
        .await?;

    assert_eq!(manager.get_tip(&session_a).await?, Some(a_message.clone()));
    assert_eq!(manager.get_tip(&session_b).await?, Some(b_message.clone()));

    let history_a = manager.get_chat_history(&session_a).await?;
    let history_b = manager.get_chat_history(&session_b).await?;

    assert_eq!(history_a.len(), 1);
    assert_eq!(history_b.len(), 1);
    assert_eq!(
        history_a.get(&a_message).unwrap().content.text(),
        Some("hello a")
    );
    assert_eq!(
        history_b.get(&b_message).unwrap().content.text(),
        Some("hello b")
    );

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn user_input_history_lists_persisted_user_messages_newest_first() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("first").into(),
            None,
        )
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::Assistant,
            String::from("assistant reply").into(),
            None,
        )
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("  second  ").into(),
            None,
        )
        .await?;

    let history = manager.list_user_input_history(10).await?;
    assert_eq!(history, vec![String::from("second"), String::from("first")]);

    let limited = manager.list_user_input_history(1).await?;
    assert_eq!(limited, vec![String::from("second")]);

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
            String::from("first prompt").into(),
            None,
        )
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::Assistant,
            String::from("assistant response").into(),
            None,
        )
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("second prompt should not replace the title").into(),
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
            String::from("before stream").into(),
            None,
        )
        .await?;
    let streaming_id = manager
        .start_streaming_message(
            &session_id,
            ChatRole::Assistant,
            StreamId::new("call-1"),
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
async fn pending_workspace_changes_are_isolated_per_session() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_a = manager.create_session().await?;
    let session_b = manager.create_session().await?;

    manager
        .set_workspace_dir(&session_a, PathBuf::from("/tmp/workspace-a"))
        .await?;

    let workspace_a = manager.get_workspace_dir_state(&session_a).await?.unwrap();
    let workspace_b = manager.get_workspace_dir_state(&session_b).await?.unwrap();

    assert_eq!(workspace_a.0, PathBuf::from("/tmp/workspace-a"));
    assert!(workspace_a.1);
    assert_eq!(workspace_b.0, PathBuf::from("/tmp/default-workspace"));
    assert!(!workspace_b.1);

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn new_sessions_inherit_last_used_profile_after_turn_starts() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let first_session = manager.create_session().await?;
    manager
        .set_session_profile(&first_session, String::from("coding"))
        .await?;
    let pending = manager
        .prepare_start_stream(
            &first_session,
            String::from("build something").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
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

    assert_eq!(inherited.selected_profile_id.as_deref(), Some("coding"));

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
            String::from("hello").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
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
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("first").into(),
            None,
        )
        .await?;
    let second_user = manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("second").into(),
            None,
        )
        .await?;
    let assistant = manager
        .add_message(
            &session_id,
            ChatRole::Assistant,
            String::from("reply").into(),
            None,
        )
        .await?;

    assert_eq!(manager.get_tip(&session_id).await?, Some(assistant));

    let restored = manager.undo_last_user_message(&session_id).await?;

    assert_eq!(restored, Some("second".into()));
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
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("coding-no-sandbox"))
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("hello").into(),
            None,
        )
        .await?;

    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("trigger failure").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    let result = ProviderManager::new()
        .generate_reply_stream(
            request.provider_id,
            &request.model_id,
            request.provider_request,
            kraai_provider_core::ProviderRequestContext::default(),
        )
        .await;
    assert!(result.is_err());
    manager.abort_streaming_message(&request.message_id).await?;

    let tip = manager.get_tip(&session_id).await?;
    let history = manager.get_chat_history(&session_id).await?;
    let latest_user_message = history
        .values()
        .find(|message| {
            message.role() == ChatRole::User && message.content.text() == Some("trigger failure")
        })
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

#[tokio::test]
async fn loading_is_passive_and_owned_recovery_restores_interrupted_stream() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("preserve this prompt").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;

    // Simulate process loss: the in-memory active-stream map vanishes, while the
    // persisted session still points at the durable streaming placeholder.
    manager.streaming_messages.write().await.clear();

    assert!(manager.prepare_session(&session_id).await?);
    assert_eq!(
        manager.get_tip(&session_id).await?,
        Some(request.message_id.clone())
    );
    manager
        .recover_interrupted_stream(manager.require_session(&session_id).await?)
        .await?;

    let history = manager.get_chat_history(&session_id).await?;
    assert_eq!(history.len(), 1);
    let user_message = history
        .values()
        .find(|message| {
            message.role() == ChatRole::User
                && message.content.text() == Some("preserve this prompt")
        })
        .expect("persisted user message");
    assert_eq!(
        manager.get_tip(&session_id).await?,
        Some(user_message.id.clone())
    );
    assert!(
        manager
            .message_store
            .get(&request.message_id)
            .await?
            .is_none()
    );

    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn loading_active_session_does_not_recover_live_stream() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("still streaming").into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;

    assert!(manager.prepare_session(&session_id).await?);
    assert_eq!(
        manager.get_tip(&session_id).await?,
        Some(request.message_id)
    );
    assert!(manager.session_has_active_stream(&session_id).await);

    cleanup_dir(data_dir).await;
    Ok(())
}
