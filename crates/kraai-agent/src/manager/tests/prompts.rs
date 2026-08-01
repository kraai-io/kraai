use super::super::*;
use super::common::{cleanup_dir, test_dir, test_manager};
use color_eyre::eyre::Result;
use kraai_persistence::{ContextStateMutation, PinnedFileScope};
use kraai_types::{CommandInvocationId, ScriptExecutionId};
use ulid::Ulid;

async fn persist_open_effect(
    manager: &mut AgentManager,
    session_id: &str,
    path: &Path,
) -> Result<()> {
    let session = manager.require_session(session_id).await?;
    let id = ScriptExecutionId::new(Ulid::generate());
    manager
        .context_state_store
        .append_command(
            session_id,
            &id,
            1,
            &CommandInvocationId::new(Ulid::generate()),
            "kraai-open-files",
            vec![ContextStateMutation::PinFile {
                path: path.to_path_buf(),
                scope: PinnedFileScope::Workspace {
                    root: session.workspace_dir,
                },
            }],
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn prepare_start_stream_injects_latest_pinned_file() -> Result<()> {
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
        .set_session_profile(&session_id, String::from("plan"))
        .await?;
    persist_open_effect(&mut manager, &session_id, &file_path).await?;
    tokio::fs::write(&file_path, "new contents\nsecond line\n").await?;

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
    assert!(system_prompt.content.contains("Opened Files"));
    assert!(system_prompt.content.contains(file_path_str.as_str()));
    assert!(system_prompt.content.contains("1|new contents"));
    assert!(system_prompt.content.contains("2|second line"));

    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn missing_pinned_file_is_durably_unpinned_and_reported_once() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = test_dir("missing-pinned-file");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    let file_path = workspace_dir.join("removed.txt");
    tokio::fs::write(&file_path, "temporary\n").await?;

    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_dir.clone())
        .await?;
    manager
        .set_session_profile(&session_id, String::from("plan"))
        .await?;
    persist_open_effect(&mut manager, &session_id, &file_path).await?;
    tokio::fs::remove_file(&file_path).await?;

    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("continue"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    assert_eq!(request.context_notifications.len(), 1);
    assert!(request.context_notifications[0].contains("automatically unpinned"));
    let system_prompt = request
        .provider_messages
        .iter()
        .rev()
        .find(|message| message.role == ChatRole::System)
        .expect("system prompt should be present");
    assert!(system_prompt.content.contains("Pinned File Updates"));
    assert!(system_prompt.content.contains("removed.txt"));
    assert!(!system_prompt.content.contains("[temporarily unavailable:"));

    let next_refresh = crate::context_state::refresh_context_state(
        manager.context_state_store.as_ref(),
        &session_id,
    )
    .await?;
    assert!(next_refresh.notifications.is_empty());
    assert!(next_refresh.prompt.is_empty());

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
        .set_session_profile(&session_id, String::from("plan"))
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
    let protocol_offset = system_prompt
        .content
        .find("# Script Execution Protocol")
        .expect("script execution protocol");
    let first_tool_offset = system_prompt
        .content
        .find("# Kraai Commands")
        .expect("command definitions");
    assert!(protocol_offset < first_tool_offset);
    assert!(
        system_prompt
            .content
            .contains("one `<tool_call>` block containing a complete Nushell script")
    );

    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn build_code_profile_includes_concise_final_answer_guidance() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;

    let session_id = manager.create_session().await?;
    manager
        .set_session_profile(&session_id, String::from("coding"))
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

    assert!(system_prompt.content.contains("Final answers"));
    assert!(
        system_prompt
            .content
            .contains("Lead with the result, not a recap of every step.")
    );
    assert!(
        system_prompt
            .content
            .contains("Do not include a mandatory \"think-ahead suggestion\"")
    );
    assert!(
        !system_prompt
            .content
            .contains("Offer at least one suggestion")
    );

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
        .set_session_profile(&session_id, String::from("plan"))
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
        .set_session_profile(&session_id, String::from("plan"))
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
        .set_session_profile(&session_id, String::from("plan"))
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
async fn prepare_continuation_injects_pinned_file() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = test_dir("open-file-continuation");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    let file_path = workspace_dir.join("notes.txt");
    tokio::fs::write(&file_path, "current\n").await?;

    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_dir.clone())
        .await?;
    manager
        .set_session_profile(&session_id, String::from("plan"))
        .await?;
    manager
        .add_message(&session_id, ChatRole::User, String::from("prior"), None)
        .await?;
    persist_open_effect(&mut manager, &session_id, &file_path).await?;

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
