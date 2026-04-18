use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;

use super::harness::{
    BatchBlockingApprovalTool, RuntimeTestHarness, ScriptedChunk, call_id_for_queue_order,
    continuation_failed_count, create_session_with_profile, stream_complete_count,
    stream_start_count,
};
use crate::Event;

#[tokio::test]
async fn overlapping_execute_requests_wait_for_in_flight_tools_from_the_same_message() -> Result<()>
{
    let started = Arc::new(tokio::sync::Notify::new());
    let ready = Arc::new(tokio::sync::Barrier::new(2));
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![
            vec![ScriptedChunk::plain(
                "<tool_call>\n\
tool: batch_blocking_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: batch_blocking_tool\n\
value: beta\n\
</tool_call>",
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ],
        {
            let started = started.clone();
            let ready = ready.clone();
            move |tools| {
                tools.register_tool(BatchBlockingApprovalTool { started, ready });
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
            String::from("run overlapping executes"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let detection_events = harness
        .events
        .wait_for("two blocking tool detections", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "batch_blocking_tool"
                    )
                })
                .count()
                == 2
        })
        .await;

    let first_call_id =
        call_id_for_queue_order(&detection_events, &session_id, "batch_blocking_tool", 0);
    let second_call_id =
        call_id_for_queue_order(&detection_events, &session_id, "batch_blocking_tool", 1);

    harness
        .handle
        .approve_tool(session_id.clone(), first_call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;
    started.notified().await;
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
        .wait_for("first overlapping tool result", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        tool_id,
                        denied,
                        ..
                    } if event_session == &session_id
                        && call_id == &first_call_id
                        && tool_id == "batch_blocking_tool"
                        && !denied
                )
            })
        })
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    let events_after_first = harness.events.snapshot();
    assert_eq!(stream_start_count(&events_after_first, &session_id), 1);
    assert_eq!(stream_complete_count(&events_after_first, &session_id), 1);

    harness
        .events
        .wait_for("both blocking results and one continuation", |events| {
            let result_count = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            tool_id,
                            denied,
                            ..
                        } if event_session == &session_id
                            && tool_id == "batch_blocking_tool"
                            && !denied
                    )
                })
                .count();
            result_count == 2 && stream_complete_count(events, &session_id) == 2
        })
        .await;

    let final_events = harness.events.snapshot();
    assert_eq!(stream_start_count(&final_events, &session_id), 2);
    assert_eq!(stream_complete_count(&final_events, &session_id), 2);
    assert_eq!(continuation_failed_count(&final_events, &session_id), 0);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn denied_and_approved_tools_finish_before_single_continuation_starts() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
        )],
        vec![ScriptedChunk::plain("continuation complete")],
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
            String::from("run approve and deny"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let detection_events = harness
        .events
        .wait_for("two tool detections for mixed decision batch", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "mock_tool"
                    )
                })
                .count()
                == 2
        })
        .await;

    let denied_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);
    let approved_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 1);

    harness
        .handle
        .deny_tool(session_id.clone(), denied_call_id.clone())
        .await?;
    harness
        .handle
        .approve_tool(session_id.clone(), approved_call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;

    harness
        .events
        .wait_for("mixed decision tool results and continuation", |events| {
            let denied_result = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        tool_id,
                        denied,
                        ..
                    } if event_session == &session_id
                        && call_id == &denied_call_id
                        && tool_id == "mock_tool"
                        && *denied
                )
            });
            let approved_result = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        tool_id,
                        denied,
                        ..
                    } if event_session == &session_id
                        && call_id == &approved_call_id
                        && tool_id == "mock_tool"
                        && !denied
                )
            });
            denied_result && approved_result && stream_complete_count(events, &session_id) == 2
        })
        .await;

    let final_events = harness.events.snapshot();
    let continuation_start_index = final_events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            Event::StreamStart {
                session_id: event_session,
                ..
            } if event_session == &session_id => Some(index),
            _ => None,
        })
        .nth(1)
        .expect("continuation stream should start once");
    let denied_result_index = final_events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::ToolResultReady {
                    session_id: event_session,
                    call_id,
                    denied,
                    ..
                } if event_session == &session_id
                    && call_id == &denied_call_id
                    && *denied
            )
        })
        .expect("denied tool result should exist");
    let approved_result_index = final_events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::ToolResultReady {
                    session_id: event_session,
                    call_id,
                    denied,
                    ..
                } if event_session == &session_id
                    && call_id == &approved_call_id
                    && !denied
            )
        })
        .expect("approved tool result should exist");

    assert!(denied_result_index < continuation_start_index);
    assert!(approved_result_index < continuation_start_index);
    assert_eq!(stream_start_count(&final_events, &session_id), 2);
    assert_eq!(stream_complete_count(&final_events, &session_id), 2);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn auto_approve_option_bypasses_manual_tool_confirmation() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
        )],
        vec![ScriptedChunk::plain("continuation complete")],
    ])
    .await
    else {
        return Ok(());
    };

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message_with_options(
            session_id.clone(),
            String::from("run manual tool without confirmation"),
            String::from("mock-model"),
            String::from("mock"),
            true,
        )
        .await?;

    harness
        .events
        .wait_for("manual tool auto-approved by option", |events| {
            let tool_result_ready = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        tool_id,
                        success,
                        denied,
                        ..
                    } if event_session == &session_id
                        && tool_id == "mock_tool"
                        && *success
                        && !denied
                )
            });
            tool_result_ready && stream_complete_count(events, &session_id) == 2
        })
        .await;

    let events = harness.events.snapshot();
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::ToolCallDetected {
                session_id: event_session,
                tool_id,
                ..
            } if event_session == &session_id && tool_id == "mock_tool"
        )
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn multiple_tools_executed_together_start_only_one_continuation() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
        )],
        vec![ScriptedChunk::plain("continuation complete")],
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
            String::from("approve all tools"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let detection_events = harness
        .events
        .wait_for("two tool detections for single execution", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "mock_tool"
                    )
                })
                .count()
                == 2
        })
        .await;

    let first_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);
    let second_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 1);

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
        .wait_for("single continuation after one execution batch", |events| {
            let first_result = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        ..
                    } if event_session == &session_id && call_id == &first_call_id
                )
            });
            let second_result = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        ..
                    } if event_session == &session_id && call_id == &second_call_id
                )
            });
            first_result && second_result && stream_complete_count(events, &session_id) == 2
        })
        .await;

    let final_events = harness.events.snapshot();
    assert_eq!(stream_start_count(&final_events, &session_id), 2);
    assert_eq!(stream_complete_count(&final_events, &session_id), 2);

    harness.shutdown().await;
    Ok(())
}
