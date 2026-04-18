use color_eyre::eyre::Result;
use kraai_tool_edit_file::EditFileTool;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn thinking_wrapped_tool_call_does_not_emit_tool_detection() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
        "<think>\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
</think>",
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
            String::from("hidden tool only"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("thinking-only response completion", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamComplete {
                        session_id: event_session,
                        ..
                    } if event_session == &session_id
                )
            })
        })
        .await;

    assert!(!harness.events.snapshot().iter().any(|event| {
        matches!(
            event,
            Event::ToolCallDetected {
                session_id: event_session,
                ..
            } if event_session == &session_id
        )
    }));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn only_visible_tool_call_is_detected_when_thinking_wraps_another() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
        "visible first\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<thinking class=\"reasoning\">\n\
<tool_call>\n\
tool: mock_tool\n\
value: hidden\n\
</tool_call>\n\
</thinking>",
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
            String::from("mixed visible and hidden tools"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("single visible tool detection", |events| {
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

    let detections = harness
        .events
        .snapshot()
        .into_iter()
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
        .count();
    assert_eq!(detections, 1);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn malformed_thinking_block_adds_history_error_and_continues() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "<think>\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
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
            String::from("malformed thinking block"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("thinking parse failure continuation", |events| {
            let stream_completions = events
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
                .count();
            stream_completions >= 2
        })
        .await;

    assert!(!harness.events.snapshot().iter().any(|event| {
        matches!(
            event,
            Event::ToolCallDetected {
                session_id: event_session,
                ..
            } if event_session == &session_id
        )
    }));

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(history.values().any(|message| {
        message.content.contains("Failed to parse thinking block")
            && message
                .content
                .contains("Missing closing </think> or </thinking> tag")
    }));
    assert!(
        history
            .values()
            .any(|message| message.content == "continuation complete")
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn invalid_tool_arguments_do_not_emit_permission_events() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![
            vec![ScriptedChunk::plain(
                r#"<tool_call>
tool: edit_file
path: /tmp/providers.toml
create: false
edits[1]{old_text,new_text}:
  old,true
</tool_call>"#,
            )],
            vec![ScriptedChunk::plain("continuation complete")],
        ],
        |tools| {
            tools.register_tool(EditFileTool);
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
            String::from("trigger invalid edit"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("invalid tool call continuation", |events| {
            let stream_completions = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::StreamComplete { session_id: event_session, .. }
                            if event_session == &session_id
                    )
                })
                .count();
            stream_completions >= 2
        })
        .await;

    assert!(!harness.events.snapshot().iter().any(|event| {
        matches!(
            event,
            Event::ToolCallDetected {
                session_id: event_session,
                ..
            } if event_session == &session_id
        )
    }));

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(history.values().any(|message| {
        message
            .content
            .contains("Unable to validate edit_file arguments")
    }));
    assert!(
        history
            .values()
            .any(|message| message.content == "continuation complete")
    );

    harness.shutdown().await;
    Ok(())
}
