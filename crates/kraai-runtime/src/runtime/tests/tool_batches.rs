use color_eyre::eyre::Result;

use super::harness::{
    RuntimeTestHarness, ScriptedChunk, call_id_for_queue_order, create_session_with_profile,
    stream_complete_count, stream_start_count,
};
use crate::Event;

#[tokio::test]
async fn denied_tool_finishes_before_single_continuation_starts() -> Result<()> {
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
        .wait_for("first tool detection for denied continuation", |events| {
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
                == 1
        })
        .await;

    let denied_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);

    harness
        .handle
        .deny_tool(session_id.clone(), denied_call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;

    harness
        .events
        .wait_for("denied tool result and continuation", |events| {
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
            denied_result && stream_complete_count(events, &session_id) == 2
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

    assert!(denied_result_index < continuation_start_index);
    assert_eq!(stream_start_count(&final_events, &session_id), 2);
    assert_eq!(stream_complete_count(&final_events, &session_id), 2);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn auto_approve_option_bypasses_profile_threshold_for_autonomous_tools() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call>\n\
tool: high_risk_auto_tool\n\
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
            String::from("run high risk autonomous tool without confirmation"),
            String::from("mock-model"),
            String::from("mock"),
            true,
        )
        .await?;

    harness
        .events
        .wait_for(
            "high risk autonomous tool auto-approved by option",
            |events| {
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
                            && tool_id == "high_risk_auto_tool"
                            && *success
                            && !denied
                    )
                });
                tool_result_ready && stream_complete_count(events, &session_id) == 2
            },
        )
        .await;

    let events = harness.events.snapshot();
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::ToolCallDetected {
                session_id: event_session,
                tool_id,
                ..
            } if event_session == &session_id && tool_id == "high_risk_auto_tool"
        )
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn auto_approve_option_does_not_bypass_always_ask_tools() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
        "<tool_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>",
    )]])
    .await
    else {
        return Ok(());
    };

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message_with_options(
            session_id.clone(),
            String::from("run explicit approval tool"),
            String::from("mock-model"),
            String::from("mock"),
            true,
        )
        .await?;

    let events = harness
        .events
        .wait_for(
            "always ask tool remains pending under auto-approve",
            |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolCallDetected {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id
                            && tool_id == "mock_tool"
                    )
                })
            },
        )
        .await;

    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::ToolResultReady {
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
async fn pending_permission_blocks_explicit_continuation_until_tool_finishes() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(concat!(
            "<tool_call>\n",
            "tool: mock_tool\n",
            "value: alpha\n",
            "</",
            "tool_call>"
        ))],
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
            String::from("run gated tool"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let detection_events = harness
        .events
        .wait_for("manual tool detection", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolCallDetected {
                        session_id: event_session,
                        tool_id,
                        ..
                    } if event_session == &session_id && tool_id == "mock_tool"
                )
            })
        })
        .await;

    harness.handle.continue_session(session_id.clone()).await?;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events_before_permission = harness.events.snapshot();
    assert_eq!(
        stream_start_count(&events_before_permission, &session_id),
        1
    );
    assert_eq!(
        stream_complete_count(&events_before_permission, &session_id),
        1
    );

    let call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);
    harness
        .handle
        .approve_tool(session_id.clone(), call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;

    harness
        .events
        .wait_for("tool result then continuation", |events| {
            let tool_result = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id: event_call_id,
                        denied,
                        ..
                    } if event_session == &session_id
                        && event_call_id == &call_id
                        && !denied
                )
            });
            tool_result && stream_complete_count(events, &session_id) == 2
        })
        .await;

    let final_events = harness.events.snapshot();
    let tool_result_index = final_events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::ToolResultReady {
                    session_id: event_session,
                    call_id: event_call_id,
                    ..
                } if event_session == &session_id && event_call_id == &call_id
            )
        })
        .expect("tool result should exist");
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
        .expect("continuation should start");

    assert!(tool_result_index < continuation_start_index);
    assert_eq!(stream_start_count(&final_events, &session_id), 2);
    assert_eq!(stream_complete_count(&final_events, &session_id), 2);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn single_tool_execution_starts_one_continuation() -> Result<()> {
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
        .wait_for("single tool detection for execution", |events| {
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
                == 1
        })
        .await;

    let first_call_id = call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);

    harness
        .handle
        .approve_tool(session_id.clone(), first_call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;

    harness
        .events
        .wait_for("single continuation after tool execution", |events| {
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
            first_result && stream_complete_count(events, &session_id) == 2
        })
        .await;

    let final_events = harness.events.snapshot();
    assert_eq!(stream_start_count(&final_events, &session_id), 2);
    assert_eq!(stream_complete_count(&final_events, &session_id), 2);

    harness.shutdown().await;
    Ok(())
}
