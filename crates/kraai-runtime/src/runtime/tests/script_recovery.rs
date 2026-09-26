use std::sync::Arc;

use color_eyre::eyre::Result;
use kraai_agent::AgentManager;
use kraai_persistence::{
    AppendMessageRequest, ConversationStore, FileMessageStore, FileSessionStore, MessageStore,
    ScriptExecutionCompletion, SessionStore,
};
use kraai_types::{ConversationItem, MessageStatus, ScriptExecutionStatus};

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;
use crate::runtime::script_execution::CompletedScriptExecution;

async fn completed_script() -> Result<Option<(RuntimeTestHarness, String, CompletedScriptExecution)>>
{
    completed_script_with_image(false).await
}

pub(super) async fn completed_script_with_image(
    include_image: bool,
) -> Result<Option<(RuntimeTestHarness, String, CompletedScriptExecution)>> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
        "<tool_call>\n# timeout=30sec permissions=workspace-write\n'changed' | save result.txt\n</tool_call>",
    )]])
    .await else {
        return Ok(None);
    };
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "change it".into(),
            "mock-model".into(),
            "mock".into(),
        )
        .await?;
    harness.events.wait_for("script approval", |events| {
        events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id: id, .. } if id == &session_id))
    }).await;
    let pending = harness
        .runtime
        .pending_script_approvals
        .lock()
        .await
        .remove(&session_id)
        .expect("pending script");
    harness
        .runtime
        .execution_store
        .mark_running(&pending.request.id)
        .await?;
    if include_image {
        use kraai_nushell_runtime::ImageAttachmentHandler;
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, image::ImageFormat::Png)?;
        let path = harness.data_dir.join("workspace/screenshot.png");
        tokio::fs::write(&path, bytes.get_ref()).await?;
        let attachments = crate::runtime::images::DurableImageAttachments {
            execution_id: pending.request.id.clone(),
            session_id: session_id.clone(),
            agent: harness.runtime.agent_manager.clone(),
            images: harness.runtime.image_store.clone(),
            executions: harness.runtime.execution_store.clone(),
        };
        attachments
            .attach(1, tokio::fs::read(&path).await?)
            .await
            .map_err(|error| color_eyre::eyre::eyre!(error))?;
        tokio::fs::remove_file(path).await?;
    }
    let record = harness
        .runtime
        .execution_store
        .finish(
            &pending.request.id,
            ScriptExecutionCompletion {
                status: ScriptExecutionStatus::Completed,
                exit_code: Some(if include_image { 1 } else { 0 }),
                sandbox_denied: false,
                error: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        )
        .await?;
    let completed = CompletedScriptExecution {
        output: harness
            .runtime
            .execution_store
            .read_output(&record.id)
            .await?,
        record,
    };
    {
        let mut manager = harness.runtime.agent_manager.write().await;
        manager
            .add_script_result_to_history(
                &session_id,
                completed.record.result_message_id.clone(),
                completed.record.profile.id.clone(),
                completed.record.call_id.clone(),
                completed.render_result()?,
            )
            .await?;
        manager.clear_active_turn(&session_id);
    }
    Ok(Some((harness, session_id, completed)))
}

pub(super) async fn reopen_agent(
    harness: &RuntimeTestHarness,
) -> Result<(Arc<FileMessageStore>, Arc<FileSessionStore>)> {
    let (messages, sessions, _, context) = kraai_persistence::init_at(&harness.data_dir).await?;
    let providers = harness
        .runtime
        .agent_manager
        .read()
        .await
        .cloned_provider_manager();
    *harness.runtime.agent_manager.write().await = AgentManager::new(
        providers,
        harness.data_dir.join("workspace"),
        messages.clone(),
        sessions.clone(),
        context,
        Arc::new(kraai_persistence::FileRequestUsageStore::new(
            &harness.data_dir,
        )),
        harness.data_dir.clone(),
    );
    Ok((messages, sessions))
}

async fn assert_undone_turn_not_replayed(retained_by_other_session: bool) -> Result<()> {
    let Some((harness, session_id, completed)) = completed_script().await? else {
        return Ok(());
    };
    if retained_by_other_session {
        let (_, sessions) = reopen_agent(&harness).await?;
        let mut retained = sessions.get(&session_id).await?.unwrap();
        retained.id = String::from("retained");
        sessions.save(&retained).await?;
    }
    assert_eq!(
        harness
            .handle
            .undo_last_user_message(session_id.clone())
            .await?,
        Some("change it".into())
    );
    assert!(
        harness
            .handle
            .get_chat_history(session_id.clone())
            .await?
            .is_empty()
    );

    let (messages, sessions) = reopen_agent(&harness).await?;
    assert_eq!(
        messages.exists(&completed.record.source_message_id).await?,
        retained_by_other_session
    );
    assert_eq!(
        messages.exists(&completed.record.result_message_id).await?,
        retained_by_other_session
    );
    assert!(sessions.get(&session_id).await?.unwrap().tip_id.is_none());
    let recovered = harness.runtime.recover_script_executions().await;
    let history = harness.handle.get_chat_history(session_id).await?;
    assert!(history.is_empty(), "replayed an undone script result");
    recovered?;
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn restart_does_not_replay_script_results_from_an_undone_turn() -> Result<()> {
    assert_undone_turn_not_replayed(false).await
}

#[tokio::test]
async fn restart_ignores_undone_script_sources_retained_by_another_session() -> Result<()> {
    assert_undone_turn_not_replayed(true).await
}

async fn append_later_message(
    harness: &RuntimeTestHarness,
    session_id: &str,
) -> Result<Arc<FileMessageStore>> {
    let (messages, sessions) = reopen_agent(harness).await?;
    ConversationStore::new(messages.clone(), sessions)
        .append_message(AppendMessageRequest {
            session_id: session_id.to_owned(),
            content: ConversationItem::User {
                content: String::from("later turn").into(),
            },
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
            title_if_first_message: None,
        })
        .await?;
    Ok(messages)
}

#[tokio::test]
async fn recovery_retains_script_sources_in_the_current_history_ancestry() -> Result<()> {
    let Some((harness, session_id, completed)) = completed_script().await? else {
        return Ok(());
    };
    append_later_message(&harness, &session_id).await?;
    reopen_agent(&harness).await?;
    let before = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(before.contains_key(&completed.record.source_message_id));
    assert!(before.contains_key(&completed.record.result_message_id));

    harness.runtime.recover_script_executions().await?;

    assert_eq!(
        serde_json::to_value(harness.handle.get_chat_history(session_id).await?)?,
        serde_json::to_value(before)?
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn recovery_still_validates_previously_delivered_script_results() -> Result<()> {
    for corrupt_output in [false, true] {
        let Some((harness, session_id, completed)) = completed_script().await? else {
            return Ok(());
        };
        let messages = append_later_message(&harness, &session_id).await?;
        if corrupt_output {
            tokio::fs::remove_file(
                harness
                    .data_dir
                    .join("executions")
                    .join(completed.record.id.as_str())
                    .join("stdout.bin"),
            )
            .await?;
        } else {
            let mut message = messages
                .get(&completed.record.result_message_id)
                .await?
                .unwrap();
            let ConversationItem::ScriptResult { output, .. } = &mut message.content else {
                panic!("expected script result");
            };
            output.0.push(kraai_types::ContentPart::Text {
                text: "changed".into(),
            });
            messages.save(&message).await?;
        }
        reopen_agent(&harness).await?;
        let error = harness
            .runtime
            .recover_script_executions()
            .await
            .expect_err("corrupt historical result");
        let expected = if corrupt_output {
            "Failed to read script output"
        } else {
            "already exists with content that does not match"
        };
        assert!(error.to_string().contains(expected), "{error:?}");
        harness.shutdown().await;
    }
    Ok(())
}
