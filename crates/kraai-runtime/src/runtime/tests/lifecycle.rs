use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use color_eyre::eyre::Result;
use kraai_types::{ChatRole, ProviderId};

use super::harness::{
    BlockingApprovalTool, DeferredFailingProvider, FailOnDemandSessionStore,
    FailOnToolMessageStore, FailingApprovalTool, RuntimeTestHarness, ScriptedChunk,
    call_id_for_queue_order, continuation_failed_count, create_session_with_profile,
    stream_complete_count,
};
use crate::Event;

#[tokio::test]
async fn start_failure_surfaces_rollback_error_without_clearing_active_turn() -> Result<()> {
    let provider_started = Arc::new(tokio::sync::Notify::new());
    let provider_release = Arc::new(tokio::sync::Notify::new());
    let fail_session_save = Arc::new(AtomicBool::new(false));
    let Some(harness) = RuntimeTestHarness::new_with_provider_and_session_store(
        Box::new(DeferredFailingProvider {
            id: ProviderId::new("mock"),
            started: provider_started.clone(),
            release: provider_release.clone(),
            failure_message: String::from("provider start failed"),
        }),
        {
            let fail_session_save = fail_session_save.clone();
            move |base_store| {
                Arc::new(FailOnDemandSessionStore {
                    inner: base_store,
                    should_fail: fail_session_save,
                })
            }
        },
    )
    .await
    else {
        return Ok(());
    };

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("trigger failure"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    provider_started.notified().await;
    fail_session_save.store(true, Ordering::SeqCst);
    provider_release.notify_one();

    let events = harness
        .events
        .wait_for("rollback failure surfaced", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::Error(message)
                        if message.contains("Failed to roll back stream")
                            && message.contains("intentional session save failure")
                )
            })
        })
        .await;

    assert!(
        events.iter().any(
            |event| matches!(event, Event::Error(message) if message == "provider start failed")
        )
    );
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::HistoryUpdated { session_id: event_session }
                if event_session == &session_id
        )
    }));

    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("retry should stay blocked"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(
        history
            .values()
            .all(|message| message.content != "retry should stay blocked")
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn continue_session_starts_new_assistant_turn_without_new_user_message() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain("first reply")],
        vec![ScriptedChunk::plain("second reply")],
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
            String::from("hello"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("first turn completion", |events| {
            stream_complete_count(events, &session_id) == 1
        })
        .await;

    harness.handle.continue_session(session_id.clone()).await?;

    harness
        .events
        .wait_for("continued turn completion", |events| {
            stream_complete_count(events, &session_id) == 2
        })
        .await;

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let user_count = history
        .values()
        .filter(|message| message.role == ChatRole::User)
        .count();
    assert_eq!(user_count, 1);
    assert!(
        history
            .values()
            .any(|message| message.content == "first reply")
    );
    assert!(
        history
            .values()
            .any(|message| message.content == "second reply")
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn continuation_failure_still_happens_once_after_all_results_in_a_tool_batch() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![vec![ScriptedChunk::plain(
            "<tool_call>\n\
tool: failing_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: failing_tool\n\
value: beta\n\
</tool_call>",
        )]],
        |tools| {
            tools.register_tool(FailingApprovalTool);
        },
    )
    .await
    else {
        return Ok(());
    };

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("run failing tools"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let detection_events = harness
        .events
        .wait_for("two failing tool detections", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "failing_tool"
                    )
                })
                .count()
                == 2
        })
        .await;

    let first_call_id = call_id_for_queue_order(&detection_events, &session_id, "failing_tool", 0);
    let second_call_id = call_id_for_queue_order(&detection_events, &session_id, "failing_tool", 1);

    harness
        .handle
        .approve_tool(session_id.clone(), first_call_id.clone())
        .await?;
    harness
        .handle
        .approve_tool(session_id.clone(), second_call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;

    harness
        .events
        .wait_for(
            "tool failures followed by one continuation failure",
            |events| {
                let result_count = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            Event::ToolResultReady {
                                session_id: event_session,
                                tool_id,
                                success,
                                denied,
                                ..
                            } if event_session == &session_id
                                && tool_id == "failing_tool"
                                && !success
                                && !denied
                        )
                    })
                    .count();
                result_count == 2 && continuation_failed_count(events, &session_id) == 1
            },
        )
        .await;

    let final_events = harness.events.snapshot();
    let continuation_failed_index = final_events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::ContinuationFailed {
                    session_id: event_session,
                    ..
                } if event_session == &session_id
            )
        })
        .expect("continuation failure should exist");
    let last_result_index = final_events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            Event::ToolResultReady {
                session_id: event_session,
                tool_id,
                success,
                denied,
                ..
            } if event_session == &session_id
                && tool_id == "failing_tool"
                && !success
                && !denied =>
            {
                Some(index)
            }
            _ => None,
        })
        .next_back()
        .expect("failing tool results should exist");

    assert!(last_result_index < continuation_failed_index);
    assert_eq!(continuation_failed_count(&final_events, &session_id), 1);

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let failed_result_count = history
        .values()
        .filter(|message| {
            message
                .content
                .contains("Tool 'failing_tool' result:\n{\n  \"error\": \"tool exploded\"\n}")
        })
        .count();
    assert_eq!(failed_result_count, 2);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn session_operations_remain_responsive_while_tool_executes() -> Result<()> {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![vec![
            ScriptedChunk::plain("before tool\n"),
            ScriptedChunk::plain(
                "<tool_call>\n\
tool: blocking_tool\n\
value: alpha\n\
</tool_call>",
            ),
        ]],
        {
            let started = started.clone();
            let release = release.clone();
            move |tools| {
                tools.register_tool(BlockingApprovalTool {
                    started,
                    release,
                    fail_message: None,
                });
            }
        },
    )
    .await
    else {
        return Ok(());
    };

    let session_a = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_a.clone(),
            String::from("start session a"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("blocking tool detection", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolCallDetected {
                        session_id,
                        tool_id,
                        ..
                    } if session_id == &session_a && tool_id == "blocking_tool"
                )
            })
        })
        .await;

    let call_id = events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallDetected {
                session_id,
                call_id,
                tool_id,
                ..
            } if session_id == &session_a && tool_id == "blocking_tool" => Some(call_id.clone()),
            _ => None,
        })
        .expect("tool call id should exist");

    let session_b = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .approve_tool(session_a.clone(), call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_a.clone())
        .await?;
    started.notified().await;

    let load_result = tokio::time::timeout(
        Duration::from_millis(200),
        harness.handle.load_session(session_b.clone()),
    )
    .await;
    assert!(matches!(load_result, Ok(Ok(true))));

    let pending_tools_result = tokio::time::timeout(
        Duration::from_millis(200),
        harness.handle.get_pending_tools(session_b.clone()),
    )
    .await;
    assert!(matches!(pending_tools_result, Ok(Ok(tools)) if tools.is_empty()));

    release.notify_waiters();

    harness
        .events
        .wait_for("blocking tool result", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id,
                        call_id: event_call_id,
                        tool_id,
                        ..
                    } if session_id == &session_a
                        && event_call_id == &call_id
                        && tool_id == "blocking_tool"
                )
            })
        })
        .await;

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn failed_tool_result_is_persisted_before_continuation_failure() -> Result<()> {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![vec![
            ScriptedChunk::plain("before tool\n"),
            ScriptedChunk::plain(
                "<tool_call>\n\
tool: blocking_tool\n\
value: alpha\n\
</tool_call>",
            ),
        ]],
        {
            let started = started.clone();
            let release = release.clone();
            move |tools| {
                tools.register_tool(BlockingApprovalTool {
                    started,
                    release,
                    fail_message: Some(String::from("tool exploded")),
                });
            }
        },
    )
    .await
    else {
        return Ok(());
    };

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("start session"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("failing tool detection", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolCallDetected {
                        session_id: event_session,
                        tool_id,
                        ..
                    } if event_session == &session_id && tool_id == "blocking_tool"
                )
            })
        })
        .await;

    let call_id = events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallDetected {
                session_id: event_session,
                call_id,
                tool_id,
                ..
            } if event_session == &session_id && tool_id == "blocking_tool" => {
                Some(call_id.clone())
            }
            _ => None,
        })
        .expect("tool call id should exist");

    harness
        .handle
        .approve_tool(session_id.clone(), call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;
    started.notified().await;
    release.notify_waiters();

    harness
        .events
        .wait_for("continuation failure after tool error", |events| {
            let tool_result_ready = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id: event_call_id,
                        tool_id,
                        success,
                        denied,
                        ..
                    } if event_session == &session_id
                        && event_call_id == &call_id
                        && tool_id == "blocking_tool"
                        && !success
                        && !denied
                )
            });
            let continuation_failed = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ContinuationFailed {
                        session_id: event_session,
                        ..
                    } if event_session == &session_id
                )
            });
            tool_result_ready && continuation_failed
        })
        .await;

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(history.values().any(|message| {
        message
            .content
            .contains("Tool 'blocking_tool' result:\n{\n  \"error\": \"tool exploded\"\n}")
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn parse_failure_history_write_error_stops_continuation_and_recovers() -> Result<()> {
    let fail_tool_history_save = Arc::new(AtomicBool::new(true));
    let Some(harness) = RuntimeTestHarness::new_with_message_store(
        vec![
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: mock_tool\n\
value: {\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("retry reply")],
        ],
        {
            let fail_tool_history_save = fail_tool_history_save.clone();
            move |base_store| {
                Arc::new(FailOnToolMessageStore {
                    inner: base_store,
                    should_fail: fail_tool_history_save,
                })
            }
        },
    )
    .await
    else {
        return Ok(());
    };

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("first message"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("parse failure history persistence error", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::ContinuationFailed {
                        session_id: event_session,
                        error,
                    } if event_session == &session_id
                        && error.contains("intentional tool history save failure")
                )
            })
        })
        .await;

    assert_eq!(stream_complete_count(&events, &session_id), 1);
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::ToolCallDetected {
                session_id: event_session,
                ..
            } if event_session == &session_id
        )
    }));

    let failed_history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(
        failed_history
            .values()
            .all(|message| !message.content.contains("Failed to parse tool call"))
    );
    assert!(
        failed_history
            .values()
            .all(|message| message.content != "retry reply")
    );

    fail_tool_history_save.store(false, Ordering::SeqCst);
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("second message"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("retry after parse failure write error", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::StreamComplete {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
                .count()
                >= 2
        })
        .await;

    let recovered_history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(
        recovered_history
            .values()
            .any(|message| message.content == "retry reply")
    );

    harness.shutdown().await;
    Ok(())
}
