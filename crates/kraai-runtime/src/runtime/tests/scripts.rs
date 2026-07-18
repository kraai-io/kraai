use color_eyre::eyre::Result;
use kraai_persistence::{FileScriptExecutionStore, ScriptExecutionStore};
use kraai_types::{ChatRole, ScriptExecutionStatus};

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn escalation_prompt_is_execution_scoped_and_denial_continues() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "I need to run the tests.\n<tool_call permissions=\"workspace-write\" timeout=\"30sec\">\n^cargo test\n</tool_call>ignored trailing output",
        )],
        vec![ScriptedChunk::plain("The requested escalation was denied.")],
    ])
    .await
    else {
        return Ok(());
    };
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("test it"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("script approval", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
            })
        })
        .await;
    let script = events
        .iter()
        .find_map(|event| match event {
            Event::ScriptApprovalRequested {
                session_id: event_session,
                script,
            } if event_session == &session_id => Some(script.clone()),
            _ => None,
        })
        .expect("approval event");
    assert_eq!(script.capability_additions, ["workspace-write"]);
    assert_eq!(script.timeout_millis, 30_000);
    assert_eq!(
        harness
            .handle
            .get_pending_script(session_id.clone())
            .await?
            .as_ref()
            .map(|pending| pending.execution_id.as_str()),
        Some(script.execution_id.as_str())
    );

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let assistant = history
        .values()
        .find(|message| message.role == ChatRole::Assistant)
        .expect("assistant script message");
    assert!(assistant.content.starts_with("I need to run the tests."));
    assert!(assistant.content.ends_with("</tool_call>"));
    assert!(!assistant.content.contains("ignored trailing output"));

    harness
        .handle
        .deny_script(session_id.clone(), script.execution_id.clone())
        .await?;
    harness
        .events
        .wait_for("continuation completion", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(event, Event::StreamComplete { session_id: event_session, .. } if event_session == &session_id)
                })
                .count()
                >= 2
        })
        .await;

    let records = FileScriptExecutionStore::new(&harness.data_dir)
        .list_for_session(&session_id)
        .await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, Some(ScriptExecutionStatus::Denied));
    let history = harness.handle.get_chat_history(session_id).await?;
    assert!(history.values().any(|message| {
        message.role == ChatRole::ToolCallResult && message.content.contains("status=\"denied\"")
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn malformed_script_is_durable_and_continues_with_invalid_result() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call permissions=\"network\">http get https://example.com</tool_call>",
        )],
        vec![ScriptedChunk::plain("The script request was invalid.")],
    ])
    .await
    else {
        return Ok(());
    };
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("fetch it"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("invalid script result", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ScriptResultReady { session_id: event_session, status, .. } if event_session == &session_id && status == "invalid-script")
            })
        })
        .await;
    let records = FileScriptExecutionStore::new(&harness.data_dir)
        .list_for_session(&session_id)
        .await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].timeout, None);
    assert_eq!(
        records[0].status,
        Some(ScriptExecutionStatus::InvalidScript)
    );
    assert!(
        records[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("timeout"))
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn recovery_finishes_orphaned_execution_delivers_one_result_and_continues() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call permissions=\"workspace-write\" timeout=\"30sec\">\n'changed' | save result.txt\n</tool_call>",
        )],
        vec![ScriptedChunk::plain(
            "The interrupted script was recovered as a runtime error.",
        )],
    ])
    .await
    else {
        return Ok(());
    };
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("change it"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("orphaned approval", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
            })
        })
        .await;
    harness
        .runtime
        .pending_script_approvals
        .lock()
        .await
        .remove(&session_id);

    harness.runtime.recover_script_executions().await?;
    harness
        .events
        .wait_for("recovery continuation", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(event, Event::StreamComplete { session_id: event_session, .. } if event_session == &session_id)
                })
                .count()
                >= 2
        })
        .await;

    let records = FileScriptExecutionStore::new(&harness.data_dir)
        .list_for_session(&session_id)
        .await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, Some(ScriptExecutionStatus::RuntimeError));
    assert!(
        records[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("AwaitingApproval"))
    );

    harness.runtime.recover_script_executions().await?;
    let history = harness.handle.get_chat_history(session_id).await?;
    assert_eq!(
        history
            .values()
            .filter(|message| message.role == ChatRole::ToolCallResult)
            .count(),
        1
    );

    harness.shutdown().await;
    Ok(())
}
