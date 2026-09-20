use super::super::*;
use super::common::{cleanup_dir, test_dir, test_manager};
use color_eyre::eyre::Result;
use kraai_types::{CommandInvocationId, ContextStateMutation, PinnedFileScope, ScriptExecutionId};
use ulid::Ulid;

fn request_prefix(request: &PendingStreamRequest) -> &str {
    request
        .provider_request
        .messages
        .first()
        .and_then(|message| match message {
            ConversationItem::System { text } => Some(text.as_str()),
            _ => None,
        })
        .expect("request should start with its instruction prefix")
}

fn request_suffix(request: &PendingStreamRequest) -> &str {
    request
        .provider_request
        .messages
        .last()
        .and_then(|message| match message {
            ConversationItem::System { text } => Some(text.as_str()),
            _ => None,
        })
        .expect("request should end with its dynamic context")
}

#[tokio::test]
async fn active_profile_survives_refresh_and_rollback_without_skipping_revalidation() -> Result<()>
{
    let (mut manager, data_dir) = test_manager().await;
    let workspace = data_dir.join("profile-workspace");
    tokio::fs::create_dir_all(workspace.join(".kraai")).await?;
    let profile_path = workspace.join(".kraai/agents.toml");
    let profile = |prompt: &str| {
        format!(
            "[[profiles]]\nid = \"turn-snapshot\"\nextends = \"plan\"\nsystem_prompt = \"{prompt}\"\n"
        )
    };
    tokio::fs::write(&profile_path, profile("ORIGINAL TURN PROMPT")).await?;
    let session = manager
        .create_session_with(Some(workspace), Some(String::from("turn-snapshot")))
        .await?;
    let first = manager
        .prepare_start_stream(
            &session,
            String::from("first"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    assert!(request_prefix(&first).contains("ORIGINAL TURN PROMPT"));
    manager.complete_message(&first.message_id).await?;
    let original = manager.script_turn_context(&session)?;
    let mut detached = manager.script_turn_context(&session)?;
    detached.profile.id.clear();
    detached.profile.commands.clear();
    assert_eq!(manager.script_turn_context(&session)?, original);

    tokio::fs::write(&profile_path, profile("REFRESHED TURN PROMPT")).await?;
    assert!(
        manager
            .prepare_intercepted_stream(
                &session,
                vec![String::from("queued")],
                ModelId::new("mock-model"),
                ProviderId::new("missing"),
            )
            .await
            .is_err()
    );
    assert_eq!(manager.get_tip(&session).await?, Some(first.message_id));
    assert_eq!(manager.script_turn_context(&session)?, original);
    let continuation = manager
        .prepare_continuation_stream(&session)
        .await?
        .expect("continuation after rollback");
    assert!(request_prefix(&continuation).contains("ORIGINAL TURN PROMPT"));
    assert!(!request_prefix(&continuation).contains("REFRESHED TURN PROMPT"));
    manager.complete_message(&continuation.message_id).await?;

    tokio::fs::write(&profile_path, "").await?;
    let error = manager
        .prepare_continuation_stream(&session)
        .await
        .expect_err("removed selected profile must still be revalidated");
    assert_eq!(
        error.to_string(),
        "Selected profile is unavailable: turn-snapshot"
    );
    assert_eq!(manager.script_turn_context(&session)?, original);

    tokio::fs::write(&profile_path, profile("REFRESHED TURN PROMPT")).await?;
    manager.clear_active_turn(&session);
    let next = manager
        .prepare_start_stream(
            &session,
            String::from("next turn"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    assert!(request_prefix(&next).contains("REFRESHED TURN PROMPT"));
    assert!(!request_prefix(&next).contains("ORIGINAL TURN PROMPT"));
    cleanup_dir(data_dir).await;
    Ok(())
}

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

    let system_prompt = request_suffix(&request);
    assert!(system_prompt.contains("Opened Files"));
    assert!(system_prompt.contains(file_path_str.as_str()));
    assert!(system_prompt.contains("1|new contents"));
    assert!(system_prompt.contains("2|second line"));
    assert!(!system_prompt.contains("# Kraai Commands"));
    assert!(request_prefix(&request).contains("# Kraai Commands"));
    assert!(!request_prefix(&request).contains("1|new contents"));

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
    assert_eq!(
        request
            .context_notifications
            .iter()
            .filter(|notification| {
                notification.contains("automatically unpinned")
                    && notification.contains("removed.txt")
            })
            .count(),
        1,
    );
    let system_prompt = request_suffix(&request);
    assert!(system_prompt.contains("Pinned File Updates"));
    assert!(system_prompt.contains("removed.txt"));
    assert!(!system_prompt.contains("[temporarily unavailable:"));

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

    let system_prompt = request_prefix(&request);
    assert!(!system_prompt.contains("Workspace Instructions"));
    assert!(!system_prompt.contains(AGENTS_MD_FILE_NAME));
    let Some(ConversationItem::System { text: prefix }) = request.provider_request.messages.first()
    else {
        return Err(eyre!("missing static prefix"));
    };
    assert!(prefix.contains("# Script Execution"));
    assert!(prefix.contains("# Kraai Commands"));
    assert!(matches!(
        request.provider_request.messages.get(1),
        Some(ConversationItem::User { .. })
    ));
    assert!(prefix.contains("one `<tool_call>` block containing the complete script input"));
    assert!(prefix.contains("end the response immediately after it"));
    assert!(prefix.contains(
        "convert their output to text with `lines` before applying row-oriented filters"
    ));
    assert!(prefix.contains(
        "Do not leave a byte stream as the final pipeline value because Nushell renders it as an unhelpful hex dump"
    ));
    assert!(prefix.contains(
        "prefer it over Nushell built-ins, external programs, or ad hoc file manipulation"
    ));
    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn coding_prefix_includes_profile_and_edit_command_guidance() -> Result<()> {
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

    let system_prompt = request_prefix(&request);

    assert!(system_prompt.contains(include_str!("../../profiles/build_code.md").trim()));
    assert!(system_prompt.contains(
        "make the smallest targeted edits that express the change instead of replacing the whole file"
    ));
    assert!(system_prompt.contains(
        "Each range is inclusive, must exist in the current file, and its old_text must exactly match"
    ));

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

    let system_prompt = request_prefix(&request);
    assert!(system_prompt.contains("Workspace Instructions"));
    assert!(system_prompt.contains("# Workspace rules"));
    assert!(system_prompt.contains("Always prefer deterministic behavior."));

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
    let first_system_prompt = request_prefix(&first_request);
    assert!(!first_system_prompt.contains("First instructions"));
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
    let second_system_prompt = request_prefix(&second_request);
    assert!(second_system_prompt.contains("First instructions"));
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
    let third_system_prompt = request_prefix(&third_request);
    assert!(third_system_prompt.contains("Updated instructions"));
    assert!(!third_system_prompt.contains("First instructions"));

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
    let system_prompt = request_prefix(&continuation);
    assert!(system_prompt.contains("Workspace A"));
    assert!(!system_prompt.contains("Workspace B"));

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
    state.active_turn_profile = Some(Arc::new(profile));

    let request = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .expect("continuation request should exist");

    let system_prompt = request_suffix(&request);
    assert!(system_prompt.contains("1|current"));
    assert!(matches!(
        request.provider_request.messages.first(),
        Some(ConversationItem::System { text })
            if text.contains("# Script Execution") && !text.contains("1|current")
    ));

    let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn skills_are_advertised_without_pinning_or_injecting_instructions() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = test_dir("skills-catalog");
    let skill_dir = workspace_dir.join(".agents/skills/review");
    tokio::fs::create_dir_all(&skill_dir).await?;
    tokio::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review changes\n---\nUNLOADED SKILL BODY",
    )
    .await?;
    let session_id = manager.create_session().await?;
    manager
        .set_workspace_dir(&session_id, workspace_dir.clone())
        .await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("review this"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    let prompt = request_prefix(&request);
    assert!(prompt.contains("workspace:review"));
    assert!(prompt.contains("Review changes"));
    assert!(!prompt.contains("UNLOADED SKILL BODY"));
    assert!(
        manager
            .context_state_store
            .list(&session_id)
            .await?
            .is_empty()
    );
    manager.complete_message(&request.message_id).await?;
    let continuation = manager.prepare_continuation_stream(&session_id).await?;
    let continuation = continuation.ok_or_else(|| eyre!("missing continuation"))?;
    assert!(request_prefix(&continuation).contains("workspace:review"));
    assert!(!request_prefix(&continuation).contains("UNLOADED SKILL BODY"));
    tokio::fs::remove_dir_all(workspace_dir).await?;
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn user_agents_md_is_layered_and_refreshed_on_continuation() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = data_dir.join("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    let user_path = data_dir.join(AGENTS_MD_FILE_NAME);
    tokio::fs::write(&user_path, "Global working agreements").await?;
    tokio::fs::write(
        workspace_dir.join(AGENTS_MD_FILE_NAME),
        "Project agreements",
    )
    .await?;
    let session_id = manager
        .create_session_with(Some(workspace_dir), None)
        .await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("first"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    let prompt = request_prefix(&request);
    assert!(prompt.contains(user_path.to_string_lossy().as_ref()));
    assert!(
        prompt.find("Global working agreements").unwrap()
            < prompt.find("Project agreements").unwrap()
    );
    assert!(prompt.contains("Workspace instructions take precedence over user-level instructions"));
    manager.complete_message(&request.message_id).await?;

    for contents in [
        "Updated global agreements",
        " \n\t",
        "Restored global agreements",
    ] {
        tokio::fs::write(&user_path, contents).await?;
        let request = manager
            .prepare_continuation_stream(&session_id)
            .await?
            .unwrap();
        let prompt = request_prefix(&request);
        assert!(!prompt.contains("Global working agreements"));
        assert!(prompt.contains("Project agreements"));
        assert_eq!(
            prompt.contains("User Instructions"),
            !contents.trim().is_empty()
        );
        if !contents.trim().is_empty() {
            assert!(prompt.contains(contents));
        }
        manager.complete_message(&request.message_id).await?;
    }
    tokio::fs::remove_file(&user_path).await?;
    let request = manager
        .prepare_continuation_stream(&session_id)
        .await?
        .unwrap();
    assert!(!request_prefix(&request).contains("User Instructions"));
    assert!(!request_prefix(&request).contains("Restored global agreements"));
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn user_agents_md_loads_without_workspace_instructions_and_reports_read_errors() -> Result<()>
{
    let (mut manager, data_dir) = test_manager().await;
    let workspace_dir = data_dir.join("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    let user_path = data_dir.join(AGENTS_MD_FILE_NAME);
    tokio::fs::write(&user_path, "Global instructions only").await?;
    let session_id = manager
        .create_session_with(Some(workspace_dir), None)
        .await?;
    let request = manager
        .prepare_start_stream(
            &session_id,
            String::from("first"),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    assert!(request_prefix(&request).contains("Global instructions only"));
    assert!(!request_prefix(&request).contains("Workspace Instructions"));
    manager.complete_message(&request.message_id).await?;
    tokio::fs::write(&user_path, [0xff]).await?;
    let error = manager
        .prepare_continuation_stream(&session_id)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains(user_path.to_string_lossy().as_ref())
    );
    cleanup_dir(data_dir).await;
    Ok(())
}
