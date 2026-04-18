use color_eyre::eyre::Result;
use std::time::Duration;

use super::harness::{
    RuntimeTestHarness, ScriptedChunk, call_id_for_queue_order, create_session_with_profile,
    stream_complete_count,
};
use crate::Event;

#[tokio::test]
async fn background_session_tool_approval_and_continuation_work_after_switch() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::plain("before tool\n"),
            ScriptedChunk::plain(
                "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
            ),
        ],
        vec![ScriptedChunk::plain("session-b reply")],
        vec![ScriptedChunk::plain("continuation complete")],
    ])
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
        .wait_for("session A tool detection", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolCallDetected {
                        session_id,
                        tool_id,
                        ..
                    } if session_id == &session_a && tool_id == "mock_tool"
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
            } if session_id == &session_a && tool_id == "mock_tool" => Some(call_id.clone()),
            _ => None,
        })
        .expect("tool call id should exist");

    let session_b = create_session_with_profile(&harness.handle, "test-profile").await?;
    assert!(harness.handle.load_session(session_b.clone()).await?);
    harness
        .handle
        .send_message(
            session_b.clone(),
            String::from("start session b"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("session B completion", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamComplete {
                        session_id,
                        ..
                    } if session_id == &session_b
                )
            })
        })
        .await;

    let session_b_tip_before = harness.handle.get_tip(session_b.clone()).await?;

    harness
        .handle
        .approve_tool(session_a.clone(), call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_a.clone())
        .await?;

    harness
        .events
        .wait_for("session A tool result and continuation", |events| {
            let tool_result_ready = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id,
                        call_id: event_call_id,
                        tool_id,
                        denied,
                        ..
                    } if session_id == &session_a
                        && event_call_id == &call_id
                        && tool_id == "mock_tool"
                        && !denied
                )
            });
            let continuation_completed = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::StreamComplete {
                            session_id,
                            ..
                        } if session_id == &session_a
                    )
                })
                .count()
                >= 2;
            tool_result_ready && continuation_completed
        })
        .await;

    let history_a = harness.handle.get_chat_history(session_a.clone()).await?;
    let history_b = harness.handle.get_chat_history(session_b.clone()).await?;
    let session_b_tip_after = harness.handle.get_tip(session_b.clone()).await?;

    assert_eq!(session_b_tip_before, session_b_tip_after);
    assert_eq!(history_b.len(), 2);
    assert!(
        history_b
            .values()
            .any(|message| message.content == "session-b reply")
    );
    assert!(
        history_a
            .values()
            .any(|message| message.content.contains("Tool 'mock_tool' result"))
    );
    assert!(
        history_a
            .values()
            .any(|message| message.content == "continuation complete")
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn continuation_waits_for_all_tools_from_one_message_across_split_executions() -> Result<()> {
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
            String::from("run two tools"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let detection_events = harness
        .events
        .wait_for("two tool detections", |events| {
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
        .execute_approved_tools(session_id.clone())
        .await?;

    harness
        .events
        .wait_for("first tool result without continuation", |events| {
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
                        && tool_id == "mock_tool"
                        && !denied
                )
            })
        })
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    let events_after_first = harness.events.snapshot();
    assert_eq!(stream_complete_count(&events_after_first, &session_id), 1);

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
        .wait_for("second tool result and single continuation", |events| {
            let second_result_ready = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        tool_id,
                        denied,
                        ..
                    } if event_session == &session_id
                        && call_id == &second_call_id
                        && tool_id == "mock_tool"
                        && !denied
                )
            });
            second_result_ready && stream_complete_count(events, &session_id) == 2
        })
        .await;

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(
        history
            .values()
            .any(|message| message.content == "continuation complete")
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn auto_approved_and_manual_tools_share_one_continuation_boundary() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call>\n\
tool: auto_tool\n\
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
            String::from("run mixed tools"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("auto result plus manual detection", |events| {
            let auto_result_ready = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        tool_id,
                        success,
                        denied,
                        ..
                    } if event_session == &session_id
                        && tool_id == "auto_tool"
                        && *success
                        && !denied
                )
            });
            let manual_detected = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolCallDetected {
                        session_id: event_session,
                        tool_id,
                        ..
                    } if event_session == &session_id && tool_id == "mock_tool"
                )
            });
            auto_result_ready && manual_detected
        })
        .await;

    let manual_call_id = call_id_for_queue_order(&events, &session_id, "mock_tool", 1);

    tokio::time::sleep(Duration::from_millis(100)).await;
    let events_after_auto = harness.events.snapshot();
    assert_eq!(stream_complete_count(&events_after_auto, &session_id), 1);

    harness
        .handle
        .approve_tool(session_id.clone(), manual_call_id.clone())
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;

    harness
        .events
        .wait_for("manual result and continuation", |events| {
            let manual_result_ready = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        call_id,
                        tool_id,
                        denied,
                        ..
                    } if event_session == &session_id
                        && call_id == &manual_call_id
                        && tool_id == "mock_tool"
                        && !denied
                )
            });
            manual_result_ready && stream_complete_count(events, &session_id) == 2
        })
        .await;

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn repeated_malformed_tool_calls_continue_without_deadlocking() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<tool_call>\n\
tool: edit_file\n\
path: Cargo.toml\n\
create: false\n\
edits: [{\"old_text\":\"rust = \\\"1.88.0\\\"\",\"new_text\":\"rust = \\\"1.90.0\\\"\"}]\n\
</tool_call>",
        )],
        vec![ScriptedChunk::plain("first continuation complete")],
        vec![ScriptedChunk::plain(
            "<tool_call>\n\
tool: edit_file\n\
path: Cargo.toml\n\
create: false\n\
edits: [{\"old_text\":\"rust = \\\"1.90.0\\\"\",\"new_text\":\"rust = \\\"1.91.0\\\"\"}]\n\
</tool_call>",
        )],
        vec![ScriptedChunk::plain("second continuation complete")],
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
            String::from("first message"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("first malformed tool call continuation", |events| {
            stream_complete_count(events, &session_id) >= 2
        })
        .await;

    let history_after_first = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(history_after_first.values().any(|message| {
        message.content.contains("Failed to parse tool call")
            && message
                .content
                .contains("Expected array length, found LeftBrace")
    }));

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
        .wait_for("second malformed tool call continuation", |events| {
            stream_complete_count(events, &session_id) >= 4
        })
        .await;

    let history_after_second = harness.handle.get_chat_history(session_id.clone()).await?;
    let parse_failure_count = history_after_second
        .values()
        .filter(|message| message.content.contains("Failed to parse tool call"))
        .count();
    assert_eq!(parse_failure_count, 2);
    assert!(
        history_after_second
            .values()
            .any(|message| message.content == "second continuation complete")
    );

    harness.shutdown().await;
    Ok(())
}
