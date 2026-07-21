use super::super::*;
use super::common::{cleanup_dir, test_manager};
use color_eyre::eyre::Result;
use kraai_types::MessageStatus;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn create_session_returns_usable_session_id() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let sessions = manager.list_sessions().await?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);
    assert_eq!(sessions[0].selected_profile_id.as_deref(), Some("plan"));
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
async fn profile_changes_are_rejected_while_turn_is_active() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("plan"))
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
async fn user_input_history_lists_persisted_user_messages_newest_first() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .add_message(&session_id, ChatRole::User, String::from("first"), None)
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::Assistant,
            String::from("assistant reply"),
            None,
        )
        .await?;
    manager
        .add_message(
            &session_id,
            ChatRole::User,
            String::from("  second  "),
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
    let context_state_store = Arc::new(kraai_persistence::FileContextStateStore::new(&data_dir));
    let manager_providers = ProviderManager::new();
    let mut manager = AgentManager::new(
        manager_providers,
        PathBuf::from("/tmp/default-workspace"),
        message_store,
        session_store,
        context_state_store,
    );

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("plan"))
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

#[tokio::test]
async fn loading_session_recovers_persisted_interrupted_stream() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("preserve this prompt"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;

    // Simulate process loss: the in-memory active-stream map and hot cache vanish, while the
    // persisted session still points at the durable streaming placeholder.
    manager.streaming_messages.write().await.clear();
    manager.message_store.unload(&request.message_id).await;

    assert!(manager.prepare_session(&session_id).await?);

    let history = manager.get_chat_history(&session_id).await?;
    assert_eq!(history.len(), 1);
    let user_message = history
        .values()
        .find(|message| message.role == ChatRole::User && message.content == "preserve this prompt")
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
            String::from("still streaming"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
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
