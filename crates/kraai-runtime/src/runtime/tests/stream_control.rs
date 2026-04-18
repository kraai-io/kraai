use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use kraai_provider_core::ProviderManager;
use kraai_tool_core::ToolManager;
use kraai_types::{ChatRole, ProviderId};

use super::harness::{
    BlockingStartProvider, RuntimeTestHarness, ScriptedChunk, create_session_with_profile,
    stream_complete_count,
};
use crate::Event;

#[tokio::test]
async fn trailing_visible_text_after_tool_call_is_truncated_and_continues() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "before tool\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n\
hallucinated tool result",
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
            String::from("trigger truncation"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("continuation after truncated suffix", |events| {
            stream_complete_count(events, &session_id) >= 2
                && events.iter().any(|event| {
                    matches!(
                        event,
                        Event::ToolResultReady {
                            session_id: event_session,
                            tool_id,
                            ..
                        } if event_session == &session_id && tool_id == "auto_tool"
                    )
                })
        })
        .await;

    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::StreamCancelled {
                session_id: event_session,
                ..
            } if event_session == &session_id
        )
    }));
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::StreamError {
                session_id: event_session,
                ..
            } if event_session == &session_id
        )
    }));

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let first_assistant = history
        .values()
        .find(|message| {
            message.role == ChatRole::Assistant && message.content.contains("tool: auto_tool")
        })
        .expect("assistant tool-call message should exist");
    assert_eq!(
        first_assistant.content,
        "before tool\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n"
    );
    assert!(
        !history
            .values()
            .any(|message| message.content.contains("hallucinated tool result"))
    );
    assert!(
        history
            .values()
            .any(|message| message.content == "continuation complete")
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn split_adjacent_tool_calls_are_both_detected() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::plain(
            "before\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
<to",
        ),
        ScriptedChunk::plain(
            "ol_call>\n\
tool: mock_tool\n\
value: beta\n\
</tool_call>\n",
        ),
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
            String::from("two tools"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
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

    assert_eq!(
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
            .count(),
        2
    );

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let assistant = history
        .values()
        .find(|message| {
            message.role == ChatRole::Assistant && message.content.contains("value: alpha")
        })
        .expect("assistant message should persist both tool calls");
    assert!(assistant.content.contains("value: alpha"));
    assert!(assistant.content.contains("value: beta"));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn visible_text_between_tool_calls_truncates_at_first_completed_tool() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
        "before\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n\
not allowed\n\
<tool_call>\n\
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
        .send_message(
            session_id.clone(),
            String::from("truncate between tools"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("single tool detection after truncation", |events| {
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
    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = harness.events.snapshot();
    assert_eq!(
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
            .count(),
        1
    );

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let assistant = history
        .values()
        .find(|message| {
            message.role == ChatRole::Assistant && message.content.contains("value: alpha")
        })
        .expect("assistant message should persist first tool only");
    assert_eq!(
        assistant.content,
        "before\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>\n"
    );
    assert!(!assistant.content.contains("value: beta"));
    assert!(!assistant.content.contains("not allowed"));

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn trailing_visible_text_split_across_chunks_still_truncates_cleanly() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::plain(
                "before\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\nha",
            ),
            ScriptedChunk::plain("llucinated continuation"),
        ],
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
            String::from("split trailing text"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("split truncation continuation", |events| {
            stream_complete_count(events, &session_id) >= 2
        })
        .await;

    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::StreamCancelled {
                session_id: event_session,
                ..
            } if event_session == &session_id
        )
    }));
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Event::StreamError {
                session_id: event_session,
                ..
            } if event_session == &session_id
        )
    }));

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let assistant = history
        .values()
        .find(|message| {
            message.role == ChatRole::Assistant && message.content.contains("tool: auto_tool")
        })
        .expect("assistant tool-call message should exist");
    assert_eq!(
        assistant.content,
        "before\n\
<tool_call>\n\
tool: auto_tool\n\
value: alpha\n\
</tool_call>\n"
    );
    assert!(
        !history
            .values()
            .any(|message| message.content.contains("hallucinated continuation"))
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn session_operations_remain_responsive_while_provider_stream_starts() -> Result<()> {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(BlockingStartProvider {
            id: ProviderId::new("mock"),
            started: started.clone(),
            release: release.clone(),
        }),
    );

    let Some(harness) = RuntimeTestHarness::new_with_parts(providers, ToolManager::new()).await
    else {
        return Ok(());
    };
    let session_a = create_session_with_profile(&harness.handle, "test-profile").await?;
    let session_b = create_session_with_profile(&harness.handle, "test-profile").await?;

    harness
        .handle
        .send_message(
            session_a.clone(),
            String::from("start session a"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;
    started.notified().await;

    let load_result = tokio::time::timeout(
        Duration::from_millis(200),
        harness.handle.load_session(session_b.clone()),
    )
    .await;
    assert!(matches!(load_result, Ok(Ok(true))));

    let tip_result = tokio::time::timeout(
        Duration::from_millis(200),
        harness.handle.get_tip(session_b.clone()),
    )
    .await;
    assert!(matches!(tip_result, Ok(Ok(None))));

    release.notify_waiters();

    harness
        .events
        .wait_for("provider stream start", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamStart { session_id, .. } if session_id == &session_a
                )
            })
        })
        .await;

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancel_stream_before_first_chunk_discards_empty_placeholder() -> Result<()> {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(BlockingStartProvider {
            id: ProviderId::new("mock"),
            started: started.clone(),
            release,
        }),
    );

    let Some(harness) = RuntimeTestHarness::new_with_parts(providers, ToolManager::new()).await
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
    started.notified().await;

    assert!(harness.handle.cancel_stream(session_id.clone()).await?);
    harness
        .events
        .wait_for("stream cancelled before first chunk", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamCancelled {
                        session_id: event_session,
                        ..
                    } if event_session == &session_id
                )
            })
        })
        .await;

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert_eq!(history.len(), 1);
    assert!(
        history
            .values()
            .all(|message| message.role == ChatRole::User)
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancel_stream_prevents_tool_detection() -> Result<()> {
    let gate = Arc::new(tokio::sync::Notify::new());
    let Some(harness) = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::plain("before tool\n"),
        ScriptedChunk::gated(
            "<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
            gate,
        ),
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
            String::from("start session"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("pre-tool chunk before cancellation", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamChunk {
                        session_id: event_session,
                        chunk,
                        ..
                    } if event_session == &session_id && chunk == "before tool\n"
                )
            })
        })
        .await;

    assert!(harness.handle.cancel_stream(session_id.clone()).await?);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = harness.events.snapshot();
    assert!(!events.iter().any(|event| {
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
async fn cancel_stream_frees_session_for_next_send() -> Result<()> {
    let gate = Arc::new(tokio::sync::Notify::new());
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::plain("partial "),
            ScriptedChunk::gated("blocked", gate),
        ],
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
            String::from("first"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("first stream chunk", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamChunk {
                        session_id: event_session,
                        chunk,
                        ..
                    } if event_session == &session_id && chunk == "partial "
                )
            })
        })
        .await;

    assert!(harness.handle.cancel_stream(session_id.clone()).await?);
    harness
        .events
        .wait_for("first cancellation", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamCancelled {
                        session_id: event_session,
                        ..
                    } if event_session == &session_id
                )
            })
        })
        .await;

    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("second"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("second stream completion", |events| {
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

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(
        history
            .values()
            .any(|message| message.content == "partial ")
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
async fn list_sessions_marks_streaming_session_while_active_and_clears_after_cancel() -> Result<()>
{
    let gate = Arc::new(tokio::sync::Notify::new());
    let Some(harness) = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::plain("partial "),
        ScriptedChunk::gated("blocked", gate),
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
            String::from("first"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("stream start", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamStart { session_id: event_session, .. } if event_session == &session_id
                )
            })
        })
        .await;

    let sessions = harness.handle.list_sessions().await?;
    assert!(
        sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| session.is_streaming)
    );

    assert!(harness.handle.cancel_stream(session_id.clone()).await?);
    harness
        .events
        .wait_for("stream cancelled", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamCancelled {
                        session_id: event_session,
                        ..
                    } if event_session == &session_id
                )
            })
        })
        .await;

    let sessions = harness.handle.list_sessions().await?;
    assert!(
        sessions
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| !session.is_streaming)
    );

    harness.shutdown().await;
    Ok(())
}
