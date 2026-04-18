use super::super::*;
use super::common::{cleanup_dir, open_file_state_delta, test_dir, test_manager};
use color_eyre::eyre::Result;
use serde_json::json;

#[tokio::test]
async fn prepare_start_stream_persists_snapshot_on_user_tip_and_injects_latest_open_file()
-> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = test_dir("open-file-start");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    let file_path = workspace_dir.join("notes.txt");
    let file_path_str = file_path.display().to_string();
    tokio::fs::write(&file_path, "old contents\n").await?;

    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_dir.clone())
        .await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
        .await?;
    manager
        .add_message(&session_id, ChatRole::User, String::from("prior"), None)
        .await?;
    manager
        .add_tool_results_to_history(
            &session_id,
            vec![ToolResult {
                call_id: CallId::new("open-call"),
                tool_id: ToolId::new("open_file"),
                output: json!({ "success": true, "path": file_path_str.clone() }),
                permission_denied: false,
                tool_state_deltas: vec![open_file_state_delta(&file_path)],
            }],
        )
        .await?;
    tokio::fs::write(&file_path, "new contents\nsecond line\n").await?;

    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("follow up"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;

    let history = manager.get_chat_history(&session_id).await?;
    let user_message = history
        .values()
        .find(|message| message.role == ChatRole::User && message.content == "follow up")
        .expect("new user message should exist");
    let snapshot = user_message
        .tool_state_snapshot
        .as_ref()
        .expect("user message should store tool state snapshot");
    assert_eq!(
        snapshot.entries["opened_files"]["paths"][0].as_str(),
        Some(file_path_str.as_str())
    );
    assert!(
        snapshot.entries["file_reads"]["by_path"][file_path_str.as_str()]
            .as_str()
            .is_some()
    );

    let system_prompt = request
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(system_prompt.content.contains("Opened Files"));
    assert!(system_prompt.content.contains(file_path_str.as_str()));
    assert!(system_prompt.content.contains("1|new contents"));
    assert!(system_prompt.content.contains("2|second line"));

    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn prepare_start_stream_omits_agents_md_when_workspace_file_is_missing() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = test_dir("agents-missing");
    tokio::fs::create_dir_all(&workspace_dir).await?;

    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_dir.clone())
        .await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
        .await?;

    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("follow up"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;

    let system_prompt = request
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(!system_prompt.content.contains("Workspace Instructions"));
    assert!(!system_prompt.content.contains(AGENTS_MD_FILE_NAME));

    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn prepare_start_stream_injects_latest_workspace_agents_md_contents() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = test_dir("agents-present");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    tokio::fs::write(
        workspace_dir.join(AGENTS_MD_FILE_NAME),
        "# Workspace rules\nAlways prefer deterministic behavior.\n",
    )
    .await?;

    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_dir.clone())
        .await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
        .await?;

    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("follow up"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;

    let system_prompt = request
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(system_prompt.content.contains("Workspace Instructions"));
    assert!(system_prompt.content.contains("# Workspace rules"));
    assert!(
        system_prompt
            .content
            .contains("Always prefer deterministic behavior.")
    );

    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn prepare_streams_re_read_workspace_agents_md_between_requests() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = test_dir("agents-dynamic");
    tokio::fs::create_dir_all(&workspace_dir).await?;

    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_dir.clone())
        .await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
        .await?;

    let first_request = manager
        .prepare_start_stream(
            &session_id,
            String::from("first"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    let first_system_prompt = first_request
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(!first_system_prompt.content.contains("First instructions"));
    manager.complete_message(&first_request.message_id).await?;

    tokio::fs::write(
        workspace_dir.join(AGENTS_MD_FILE_NAME),
        "First instructions\n",
    )
    .await?;

    let second_request = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("continuation request should exist");
    let second_system_prompt = second_request
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(second_system_prompt.content.contains("First instructions"));
    manager.complete_message(&second_request.message_id).await?;

    tokio::fs::write(
        workspace_dir.join(AGENTS_MD_FILE_NAME),
        "Updated instructions\n",
    )
    .await?;

    let third_request = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("continuation request should exist");
    let third_system_prompt = third_request
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(third_system_prompt.content.contains("Updated instructions"));
    assert!(!third_system_prompt.content.contains("First instructions"));

    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn continuation_uses_active_workspace_agents_md_when_workspace_change_is_pending()
-> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_a = test_dir("agents-active-workspace-a");
    let workspace_b = test_dir("agents-active-workspace-b");
    tokio::fs::create_dir_all(&workspace_a).await?;
    tokio::fs::create_dir_all(&workspace_b).await?;
    tokio::fs::write(workspace_a.join(AGENTS_MD_FILE_NAME), "Workspace A\n").await?;
    tokio::fs::write(workspace_b.join(AGENTS_MD_FILE_NAME), "Workspace B\n").await?;

    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_a.clone())
        .await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
        .await?;

    let first_request = manager
        .prepare_start_stream(
            &session_id,
            String::from("first"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.complete_message(&first_request.message_id).await?;

    manager
        .set_workspace_dir(&session_id, workspace_b.clone())
        .await?;

    let continuation = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("continuation request should exist");
    let system_prompt = continuation
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(system_prompt.content.contains("Workspace A"));
    assert!(!system_prompt.content.contains("Workspace B"));

    let workspace_state = manager.get_workspace_dir_state(&session_id).await?.unwrap();
    assert_eq!(workspace_state.0, workspace_b);
    assert!(workspace_state.1);

    let _ = tokio::fs::remove_dir_all(&workspace_a).await;
    let _ = tokio::fs::remove_dir_all(&workspace_b).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn prepare_continuation_persists_snapshot_on_tool_tip() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = test_dir("open-file-continuation");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    let file_path = workspace_dir.join("notes.txt");
    let file_path_str = file_path.display().to_string();
    tokio::fs::write(&file_path, "current\n").await?;

    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_dir.clone())
        .await?;
    manager
        .set_session_profile(&session_id, String::from("plan-code"))
        .await?;
    manager
        .add_message(&session_id, ChatRole::User, String::from("prior"), None)
        .await?;
    manager
        .add_tool_results_to_history(
            &session_id,
            vec![ToolResult {
                call_id: CallId::new("open-call"),
                tool_id: ToolId::new("open_file"),
                output: json!({ "success": true, "path": file_path_str.clone() }),
                permission_denied: false,
                tool_state_deltas: vec![open_file_state_delta(&file_path)],
            }],
        )
        .await?;

    let session = manager.require_session(&session_id).await?;
    let profile = manager.resolve_selected_profile(&session)?;
    let state = manager.ensure_runtime_state(&session_id, &session.workspace_dir);
    state.last_model = Some(ModelId::new("mock-model"));
    state.last_provider = Some(ProviderId::new("mock"));
    state.active_turn_profile = Some(profile);

    let request = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("continuation request should exist");

    let history = manager.get_chat_history(&session_id).await?;
    let tool_message = history
        .values()
        .find(|message| message.role == ChatRole::Tool && !message.tool_state_deltas.is_empty())
        .expect("tool result message should exist");
    let snapshot = tool_message
        .tool_state_snapshot
        .as_ref()
        .expect("tool result message should store tool state snapshot");
    assert_eq!(
        snapshot.entries["opened_files"]["paths"][0].as_str(),
        Some(file_path_str.as_str())
    );
    assert!(
        snapshot.entries["file_reads"]["by_path"][file_path_str.as_str()]
            .as_str()
            .is_some()
    );

    let system_prompt = request
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(system_prompt.content.contains("1|current"));

    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}
