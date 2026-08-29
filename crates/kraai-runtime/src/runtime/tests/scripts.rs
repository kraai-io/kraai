use color_eyre::eyre::Result;
use kraai_persistence::{FileScriptExecutionStore, ScriptExecutionStore};
use kraai_types::{ChatRole, ScriptExecutionPhase, ScriptExecutionStatus, TokenUsage};

use super::super::streaming::POST_BOUNDARY_DRAIN_YIELD_INTERVAL;
use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[test]
fn interrupted_recovery_status_matches_the_last_phase() {
    use super::super::scripts::interrupted_execution_outcome;

    for (phase, expected) in [
        (
            ScriptExecutionPhase::Prepared,
            ScriptExecutionStatus::FailedToStart,
        ),
        (
            ScriptExecutionPhase::AwaitingApproval,
            ScriptExecutionStatus::Cancelled,
        ),
        (
            ScriptExecutionPhase::Running,
            ScriptExecutionStatus::RuntimeError,
        ),
    ] {
        assert_eq!(interrupted_execution_outcome(phase).0, expected);
    }
}

#[tokio::test]
async fn escalation_prompt_is_execution_scoped_and_denial_continues() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::plain(
                "I need to run the tests.\n<tool_call permissions=\"workspace-write\" timeout=\"30sec\">\n^cargo test\n</tool_call>ignored in-boundary chunk",
            ),
            ScriptedChunk::plain("ignored trailing output"),
            ScriptedChunk::usage(TokenUsage {
                total_tokens: 42,
                input_tokens: 20,
                output_tokens: 12,
                reasoning_tokens: 6,
                cache_read_tokens: 4,
            }),
        ],
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
    assert!(!assistant.content.contains("ignored in-boundary chunk"));
    assert!(!assistant.content.contains("ignored trailing output"));
    assert_eq!(
        assistant
            .generation
            .as_ref()
            .and_then(|generation| generation.usage.as_ref())
            .map(|usage| usage.total_tokens),
        Some(42)
    );

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
async fn post_boundary_drain_preserves_usage_after_many_trailing_events() -> Result<()> {
    let mut chunks = vec![ScriptedChunk::plain(
        "<tool_call permissions=\"workspace-write\" timeout=\"30sec\">\n^cargo test\n</tool_call>",
    )];
    chunks.extend(
        std::iter::repeat_with(|| ScriptedChunk::plain("discarded trailing output"))
            .take(POST_BOUNDARY_DRAIN_YIELD_INTERVAL),
    );
    chunks.push(ScriptedChunk::usage(TokenUsage {
        total_tokens: 42,
        input_tokens: 20,
        output_tokens: 12,
        reasoning_tokens: 6,
        cache_read_tokens: 4,
    }));
    let Some(harness) = RuntimeTestHarness::new(vec![chunks]).await else {
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

    harness
        .events
        .wait_for("bounded-drain script approval", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
            })
        })
        .await;
    let history = harness.handle.get_chat_history(session_id).await?;
    let assistant = history
        .values()
        .find(|message| message.role == ChatRole::Assistant)
        .expect("assistant script message");
    assert!(!assistant.content.contains("discarded trailing output"));
    assert_eq!(
        assistant
            .generation
            .as_ref()
            .and_then(|generation| generation.usage.as_ref())
            .map(|usage| usage.total_tokens),
        Some(42)
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn post_boundary_drain_error_preserves_completed_script() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::plain(
            "<tool_call permissions=\"workspace-write\" timeout=\"30sec\">\n^cargo test\n</tool_call>",
        ),
        ScriptedChunk::error("transport failed after completed script"),
    ]])
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
        .wait_for("script approval or stream error", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
                    || matches!(event, Event::StreamError { session_id: event_session, .. } if event_session == &session_id)
            })
        })
        .await;
    assert!(events.iter().any(|event| {
        matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
    }));
    assert!(!events.iter().any(|event| {
        matches!(event, Event::StreamError { session_id: event_session, .. } if event_session == &session_id)
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn pre_boundary_stream_error_remains_a_failure() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![ScriptedChunk::error(
        "transport failed before completed script",
    )]])
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
        .wait_for("pre-boundary stream error", |events| {
            events.iter().any(|event| {
                matches!(event, Event::StreamError { session_id: event_session, .. } if event_session == &session_id)
            })
        })
        .await;
    assert!(!events.iter().any(|event| {
        matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
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
            "The pending approval was cancelled without running the script.",
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
    assert_eq!(records[0].status, Some(ScriptExecutionStatus::Cancelled));
    assert!(
        records[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("awaiting approval") && error.contains("not run"))
    );

    harness.runtime.recover_script_executions().await?;
    let history = harness.handle.get_chat_history(session_id).await?;
    let result = history
        .values()
        .find(|message| message.role == ChatRole::ToolCallResult)
        .expect("recovered script result");
    assert!(result.content.contains("status=\"cancelled\""));
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
