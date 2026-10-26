use color_eyre::eyre::Result;
use kraai_execution_types::{ScriptExecutionPhase, ScriptExecutionStatus};
use kraai_persistence::{ScriptExecutionStore, SqliteScriptExecutionStore};
use kraai_types::{AssistantItem, AssistantPhase, ChatRole, ScriptCall, TokenUsage};

use super::super::stream_driver::POST_BOUNDARY_DRAIN_YIELD_INTERVAL;
use super::harness::FullHistory as _;
use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[test]
fn interrupted_recovery_status_matches_the_last_phase() {
    use super::super::script_recovery::interrupted_execution_outcome;

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
async fn native_custom_call_preserves_phase_call_identity_and_usage() -> Result<()> {
    let reasoning = serde_json::json!({"type":"reasoning","id":"rs-1","encrypted_content":"opaque-ciphertext","summary":[]});
    let Some(harness) = RuntimeTestHarness::new_native(vec![
        vec![
            ScriptedChunk::reasoning(reasoning.clone()),
            ScriptedChunk::commentary("I will inspect it."),
            ScriptedChunk::native_call(
                "openai-call-1",
                "# timeout=30sec permissions=workspace-write\n^cargo test",
            ),
            ScriptedChunk::usage(TokenUsage {
                total_tokens: 25,
                input_tokens: 12,
                output_tokens: 8,
                reasoning_tokens: 5,
                cache_read_tokens: 0,
                ..Default::default()
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
            String::from("mock-native"),
            Default::default(),
        )
        .await?;

    let events = harness
        .events
        .wait_for("native script approval", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
            })
        })
        .await;
    let execution_id = events
        .iter()
        .find_map(|event| match event {
            Event::ScriptApprovalRequested {
                session_id: event_session,
                script,
            } if event_session == &session_id => Some(script.execution_id.clone()),
            _ => None,
        })
        .expect("native approval event");

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let assistant = history
        .values()
        .find(|message| message.role() == ChatRole::Assistant)
        .expect("native assistant message");
    let items = assistant
        .content
        .assistant_items()
        .expect("assistant items");
    assert!(matches!(
        items.first(),
        Some(AssistantItem::Reasoning { provider_id, payload }) if provider_id.as_str() == "mock-native" && payload.is_null()
    ));
    let stored = harness
        .runtime
        .agent_manager
        .get_chat_history(&session_id)
        .await?
        .remove(&assistant.id)
        .expect("stored assistant message");
    assert!(matches!(
        stored.content.assistant_items().and_then(<[_]>::first),
        Some(AssistantItem::Reasoning { payload, .. }) if payload == &reasoning
    ));
    assert!(
        !assistant
            .content
            .display_text()
            .contains("opaque-ciphertext")
    );
    assert!(matches!(
        items.get(1),
        Some(AssistantItem::Text {
            phase: AssistantPhase::Commentary,
            text,
        }) if text == "I will inspect it."
    ));
    assert!(matches!(
        items.get(2),
        Some(AssistantItem::ScriptCall(ScriptCall { call_id, name, input, arguments_error: None }))
            if call_id.as_str() == "openai-call-1"
                && name == "kraai_nushell"
                && input.starts_with("# timeout=30sec")
    ));
    assert_eq!(
        assistant
            .generation
            .as_ref()
            .and_then(|generation| generation.usage.as_ref())
            .map(|usage| usage.total_tokens),
        Some(25)
    );

    harness
        .handle
        .deny_script(session_id.clone(), execution_id)
        .await?;
    harness
        .events
        .wait_for("native continuation", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(event, Event::StreamComplete { session_id: event_session, .. } if event_session == &session_id)
                })
                .count()
                >= 2
        })
        .await;

    let records = SqliteScriptExecutionStore::new(&harness.data_dir)
        .list_for_session(&session_id)
        .await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].call_id.as_str(), "openai-call-1");

    let history = harness.handle.get_chat_history(session_id).await?;
    assert!(history.values().any(|message| {
        matches!(
            &message.content,
            kraai_types::ConversationItem::ScriptResult { call_id, .. }
                if call_id.as_str() == "openai-call-1"
        )
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn escalation_prompt_is_execution_scoped_and_denial_continues() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::plain("I need to run the tests."),
            ScriptedChunk::native_call(
                "call-1",
                "# timeout=30sec permissions=workspace-write\n^cargo test",
            ),
            ScriptedChunk::plain("ignored trailing output"),
            ScriptedChunk::usage(TokenUsage {
                total_tokens: 42,
                input_tokens: 20,
                output_tokens: 12,
                reasoning_tokens: 6,
                cache_read_tokens: 4,
                ..Default::default()
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
            Default::default(),
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
        .find(|message| message.role() == ChatRole::Assistant)
        .expect("assistant script message");
    let assistant_text = assistant.display_text();
    assert!(assistant_text.starts_with("I need to run the tests."));
    assert!(assistant_text.ends_with("</tool_call>"));
    assert!(!assistant_text.contains("ignored in-boundary chunk"));
    assert!(!assistant_text.contains("ignored trailing output"));
    assert_eq!(
        assistant
            .generation
            .as_ref()
            .and_then(|generation| generation.usage.as_ref())
            .map(|usage| usage.total_tokens),
        Some(42)
    );

    let (_, paused_timer) = harness.runtime.event_tx.timer_snapshot(&session_id);
    assert!(
        harness
            .handle
            .delete_session(session_id.clone())
            .await
            .is_err()
    );
    assert_eq!(
        harness.runtime.event_tx.timer_snapshot(&session_id).1,
        paused_timer
    );
    let barrier = harness.runtime.sessions.barrier(&session_id);
    let continuation_guard = barrier.write().await;
    harness
        .handle
        .deny_script(session_id.clone(), script.execution_id.clone())
        .await?;
    let delay_started = std::time::Instant::now();
    let elapsed_before_delay = harness
        .runtime
        .event_tx
        .timer_snapshot(&session_id)
        .1
        .elapsed(delay_started)
        .expect("active timer");
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    let delay_finished = std::time::Instant::now();
    let elapsed_after_delay = harness
        .runtime
        .event_tx
        .timer_snapshot(&session_id)
        .1
        .elapsed(delay_finished)
        .expect("active timer");
    assert_eq!(
        elapsed_after_delay - elapsed_before_delay,
        delay_finished - delay_started
    );
    drop(continuation_guard);
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

    let snapshot = harness
        .handle
        .get_session_snapshot(session_id.clone())
        .await?;
    assert!(snapshot.turn_timer.last_duration() >= Some(elapsed_after_delay));

    let records = SqliteScriptExecutionStore::new(&harness.data_dir)
        .list_for_session(&session_id)
        .await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, Some(ScriptExecutionStatus::Denied));
    let history = harness.handle.get_chat_history(session_id).await?;
    assert!(history.values().any(|message| {
        message.role() == ChatRole::ToolCallResult
            && message.display_text().contains("status=\"denied\"")
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn post_boundary_drain_preserves_usage_after_many_trailing_events() -> Result<()> {
    let mut chunks = vec![ScriptedChunk::native_call(
        "call-1",
        "# timeout=30sec permissions=workspace-write\n^cargo test",
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
        ..Default::default()
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
            Default::default(),
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
        .find(|message| message.role() == ChatRole::Assistant)
        .expect("assistant script message");
    assert!(
        !assistant
            .display_text()
            .contains("discarded trailing output")
    );
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
        ScriptedChunk::native_call(
            "call-1",
            "# timeout=30sec permissions=workspace-write\n^cargo test",
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
            Default::default(),
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
    let Some(harness) = RuntimeTestHarness::new(vec![vec![ScriptedChunk::error_with_source(
        "error decoding response body",
        "operation timed out",
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
            Default::default(),
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
    assert!(events.iter().any(|event| {
        matches!(event, Event::StreamError { session_id: event_session, message_id, error }
            if event_session == &session_id && error.contains(message_id)
                && error.contains("ms: error decoding response body: operation timed out"))
    }));
    assert!(!events.iter().any(|event| {
        matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn malformed_script_is_durable_and_continues_with_invalid_result() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::native_call(
            "call-1",
            "# permissions=network\nhttp get https://example.com",
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
            Default::default(),
        )
        .await?;

    harness
        .events
        .wait_for("invalid script result", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ScriptResultReady { session_id: event_session, outcome, .. } if event_session == &session_id && outcome.status == ScriptExecutionStatus::InvalidScript)
            })
        })
        .await;
    let records = SqliteScriptExecutionStore::new(&harness.data_dir)
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
async fn invalid_tool_call_fails_without_running_and_replays_raw_call() -> Result<()> {
    let error = "unknown tool 'bash'; the only tool is 'kraai_nushell'";
    let arguments = r#"{"command": "ls"}"#;
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::invalid_call(
            "call-1", "bash", arguments, error,
        )],
        vec![ScriptedChunk::plain("Retrying with the right tool.")],
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
            String::from("list files"),
            String::from("mock-model"),
            String::from("mock"),
            Default::default(),
        )
        .await?;

    let events = harness
        .events
        .wait_for("turn completion", |events| {
            events.iter().any(|event| {
                matches!(event, Event::TurnCompleted { session_id: event_session } if event_session == &session_id)
            })
        })
        .await;
    let diagnostic = format!("invalid tool call: {error}");
    assert!(events.iter().any(|event| {
        matches!(event, Event::ScriptResultReady { session_id: event_session, call_id, output, outcome, .. }
            if event_session == &session_id
                && call_id == "call-1"
                && outcome.status == ScriptExecutionStatus::InvalidScript
                && output.display_text().contains(&diagnostic))
    }));
    assert!(!events.iter().any(|event| {
        matches!(event, Event::ScriptApprovalRequested { session_id: event_session, .. } if event_session == &session_id)
    }));

    let requests = harness
        .provider_requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(requests.len(), 2);
    let retry = &requests[1].messages;
    let call = retry
        .iter()
        .position(|item| {
            item.assistant_items().is_some_and(|items| {
                items.contains(&AssistantItem::ScriptCall(ScriptCall {
                    call_id: kraai_types::ToolCallId::new("call-1"),
                    name: String::from("bash"),
                    input: String::from(arguments),
                    arguments_error: Some(String::from(error)),
                }))
            })
        })
        .ok_or_else(|| color_eyre::eyre::eyre!("retry request is missing the invalid call"))?;
    assert!(matches!(
        retry.get(call + 1),
        Some(kraai_types::ConversationItem::ScriptResult { call_id, output, .. })
            if call_id.as_str() == "call-1" && output.display_text().contains(&diagnostic)
    ));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn recovery_finishes_orphaned_execution_delivers_one_result_and_continues() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::native_call(
            "call-1",
            "# timeout=30sec permissions=workspace-write\n'changed' | save result.txt",
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
            Default::default(),
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
    super::harness::take_pending_approval(&harness.runtime, &session_id).await;

    harness
        .runtime
        .recover_session_executions(&session_id)
        .await?;
    harness
        .runtime
        .with_session_actor(&session_id, |actor| {
            Box::pin(async move { actor.continue_turn().await })
        })
        .await;
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

    let records = SqliteScriptExecutionStore::new(&harness.data_dir)
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

    harness
        .runtime
        .session_store
        .claim_turn(&session_id)
        .await?;
    harness
        .runtime
        .recover_session_executions(&session_id)
        .await?;
    let history = harness.handle.get_chat_history(session_id).await?;
    let result = history
        .values()
        .find(|message| message.role() == ChatRole::ToolCallResult)
        .expect("recovered script result");
    assert!(result.display_text().contains("status=\"cancelled\""));
    assert_eq!(
        history
            .values()
            .filter(|message| message.role() == ChatRole::ToolCallResult)
            .count(),
        1
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn transient_pre_boundary_failure_reissues_the_request() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::plain("partial answer"),
            ScriptedChunk::interrupted("connection reset"),
        ],
        vec![ScriptedChunk::plain("complete answer")],
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
            String::from("answer"),
            String::from("mock-model"),
            String::from("mock"),
            Default::default(),
        )
        .await?;
    let events = harness
        .events
        .wait_for("reissued stream completion", |events| {
            events.iter().any(|event| {
                matches!(event, Event::StreamComplete { session_id: id, .. } if id == &session_id)
                    || matches!(event, Event::StreamError { session_id: id, .. } if id == &session_id)
            })
        })
        .await;
    assert!(events.iter().any(|event| {
        matches!(event, Event::StreamRestarted { session_id: id, .. } if id == &session_id)
    }));
    assert!(!events.iter().any(|event| {
        matches!(event, Event::StreamError { session_id: id, .. } if id == &session_id)
    }));
    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let replies: Vec<_> = history
        .values()
        .filter(|message| message.role() == kraai_types::ChatRole::Assistant)
        .map(|message| message.display_text().into_owned())
        .collect();
    assert_eq!(replies, vec![String::from("complete answer")]);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_runtime_that_lost_the_session_does_not_reissue_the_request() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::plain("partial answer"),
            ScriptedChunk::interrupted("connection reset"),
        ],
        vec![ScriptedChunk::plain("answer for the new owner")],
    ])
    .await
    .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let agent = &harness.runtime.agent_manager;
    let request = agent
        .prepare_start_stream(
            &session_id,
            String::from("answer").into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    let store = &harness.runtime.session_store;
    if !store.owns_turn(&session_id).await? {
        store.claim_turn(&session_id).await?;
    }
    store.release_turn(&session_id).await?;
    let result = super::harness::drive_stream(
        harness.runtime.clone(),
        session_id.clone(),
        agent.cloned_provider_manager(),
        request,
    )
    .await;
    let super::super::stream_driver::StreamDriveResult::FailedDuringStream { error } = result
    else {
        return Err(color_eyre::eyre::eyre!(
            "a deposed runtime reissued the request: {result:?}"
        ));
    };
    assert!(error.contains("owned by another instance"), "{error}");
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn long_script_output_is_excerpted_and_saved_where_scripts_can_read_it() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::native_call(
            "call-1",
            "# timeout=30sec permissions=workspace-write\n'changed' | save result.txt",
        )],
        vec![ScriptedChunk::plain("done")],
    ])
    .await
    .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "change it".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    harness
        .events
        .wait_for("script approval", |events| {
            events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id: id, .. } if id == &session_id))
        })
        .await;
    let output_dir = harness.runtime.config.script_output_dir(&session_id);
    let stdout: String = (0..5000).map(|line| format!("line {line}\n")).collect();
    let full_output = stdout.clone();
    let request = harness
        .runtime
        .with_session_actor(&session_id, move |actor| {
            Box::pin(async move {
                let pending = actor.take_pending_approval().expect("pending script");
                let store = actor.runtime.execution_store.clone();
                store
                    .mark_running(&pending.request.id)
                    .await
                    .expect("start execution");
                let record = store
                    .finish(
                        &pending.request.id,
                        kraai_persistence::ScriptExecutionCompletion {
                            status: ScriptExecutionStatus::Completed,
                            exit_code: Some(0),
                            sandbox_denied: false,
                            error: None,
                            stdout: stdout.into_bytes(),
                            stderr: Vec::new(),
                        },
                    )
                    .await
                    .expect("finish execution");
                let output = store.read_output(&record.id).await.expect("read output");
                actor
                    .finish_script(super::super::script_execution::CompletedScriptExecution {
                        record,
                        output,
                    })
                    .await
                    .expect("finish script");
                pending.request
            })
        })
        .await;
    assert!(request.runtime_roots.contains(&output_dir));

    let events = harness
        .events
        .wait_for("script result", |events| {
            events.iter().any(|event| matches!(event, Event::ScriptResultReady { session_id: id, .. } if id == &session_id))
        })
        .await;
    let rendered = events
        .iter()
        .find_map(|event| match event {
            Event::ScriptResultReady { output, .. } => output.as_text().map(str::to_string),
            _ => None,
        })
        .expect("rendered result");
    let saved = output_dir.join(format!("{}.stdout", request.id));
    assert!(rendered.len() < full_output.len() / 2);
    assert!(rendered.contains(&format!("full_output=\"{}\"", saved.display())));
    assert!(rendered.contains("bytes omitted]"));
    assert_eq!(tokio::fs::read_to_string(&saved).await?, full_output);

    harness
        .events
        .wait_for("turn completed", |events| {
            events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id: id } if id == &session_id))
        })
        .await;
    harness.handle.delete_session(session_id).await?;
    assert!(!tokio::fs::try_exists(&output_dir).await?);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn only_the_first_script_call_of_a_response_runs_and_the_others_are_answered() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::native_call(
                "call-1",
                "# timeout=30sec permissions=workspace-write\n^cargo test",
            ),
            ScriptedChunk::native_call("call-2", "# timeout=30sec\nls"),
        ],
        vec![ScriptedChunk::plain("Both calls are answered.")],
    ])
    .await
    .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "test it".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    harness
        .events
        .wait_for("approval of the first call", |events| {
            events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id: id, .. } if id == &session_id))
        })
        .await;
    let pending = harness
        .handle
        .get_pending_script(session_id.clone())
        .await?
        .expect("pending script");
    assert_eq!(pending.call_id, "call-1");
    harness
        .handle
        .deny_script(session_id.clone(), pending.execution_id)
        .await?;
    harness
        .events
        .wait_for("continuation", |events| {
            events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id: id } if id == &session_id))
        })
        .await;

    let history = harness
        .runtime
        .agent_manager
        .get_chat_history(&session_id)
        .await?;
    let results: Vec<_> = history
        .values()
        .filter_map(|message| match &message.content {
            kraai_types::ConversationItem::ScriptResult {
                call_id, outcome, ..
            } => Some((call_id.as_str().to_string(), outcome.status)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(results.contains(&("call-1".into(), ScriptExecutionStatus::Denied)));
    assert!(results.contains(&("call-2".into(), ScriptExecutionStatus::Cancelled)));
    let calls = history
        .values()
        .filter_map(|message| message.content.assistant_items())
        .flatten()
        .filter(|item| matches!(item, AssistantItem::ScriptCall(_)))
        .count();
    assert_eq!(calls, 2);
    harness.shutdown().await;
    Ok(())
}
