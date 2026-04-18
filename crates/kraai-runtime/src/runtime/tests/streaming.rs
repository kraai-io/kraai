use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use color_eyre::eyre::Result;

use super::harness::{
    FailOnAssistantCompletionMessageStore, RuntimeTestHarness, ScriptedChunk,
    create_session_with_profile, stream_complete_count, stream_complete_for,
};
use crate::Event;

#[tokio::test]
async fn background_session_stream_continues_after_switch() -> Result<()> {
    let gate = Arc::new(tokio::sync::Notify::new());
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![
            ScriptedChunk::plain("session-a chunk 1 "),
            ScriptedChunk::gated("session-a chunk 2", gate.clone()),
        ],
        vec![ScriptedChunk::plain("session-b complete")],
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

    harness
        .events
        .wait_for("session A first chunk", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamChunk {
                        session_id,
                        chunk,
                        ..
                    } if session_id == &session_a && chunk == "session-a chunk 1 "
                )
            })
        })
        .await;

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

    let events = harness
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

    assert!(
        !events.iter().any(|event| {
            matches!(
                event,
                Event::StreamComplete {
                    session_id,
                    ..
                } if session_id == &session_a
            )
        }),
        "session A should still be streaming while session B completes"
    );

    gate.notify_one();

    let events = harness
        .events
        .wait_for("session A completion", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamComplete {
                        session_id,
                        ..
                    } if session_id == &session_a
                )
            })
        })
        .await;

    assert!(stream_complete_for(&events, &session_b) < stream_complete_for(&events, &session_a));

    let history_a = harness.handle.get_chat_history(session_a.clone()).await?;
    let history_b = harness.handle.get_chat_history(session_b.clone()).await?;
    assert_eq!(history_a.len(), 2);
    assert_eq!(history_b.len(), 2);
    assert!(
        history_a
            .values()
            .any(|message| message.content == "session-a chunk 1 session-a chunk 2")
    );
    assert!(
        history_b
            .values()
            .any(|message| message.content == "session-b complete")
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn completion_save_failure_rolls_back_stream_and_allows_retry() -> Result<()> {
    let fail_completion_save = Arc::new(AtomicBool::new(true));
    let Some(harness) = RuntimeTestHarness::new_with_message_store(
        vec![
            vec![ScriptedChunk::plain("first reply")],
            vec![ScriptedChunk::plain("second reply")],
        ],
        {
            let fail_completion_save = fail_completion_save.clone();
            move |base_store| {
                Arc::new(FailOnAssistantCompletionMessageStore {
                    inner: base_store,
                    should_fail: fail_completion_save,
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
            String::from("first prompt"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("stream completion persistence failure", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamError {
                        session_id: event_session,
                        error,
                        ..
                    } if event_session == &session_id
                        && error.contains("intentional assistant completion save failure")
                )
            })
        })
        .await;

    assert_eq!(stream_complete_count(&events, &session_id), 0);

    let failed_history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert_eq!(failed_history.len(), 1);
    assert!(
        failed_history
            .values()
            .all(|message| message.content != "first reply")
    );

    fail_completion_save.store(false, Ordering::SeqCst);
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("second prompt"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("retry completion after rollback", |events| {
            stream_complete_count(events, &session_id) == 1
        })
        .await;

    let recovered_history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(
        recovered_history
            .values()
            .any(|message| message.content == "second reply")
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancel_stream_persists_partial_message_as_complete() -> Result<()> {
    let gate = Arc::new(tokio::sync::Notify::new());
    let Some(harness) = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::plain("partial "),
        ScriptedChunk::gated("more text", gate),
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
        .wait_for("first chunk before cancellation", |events| {
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
        .wait_for("stream cancelled event", |events| {
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
    let cancelled_message = history
        .values()
        .find(|message| message.role == kraai_types::ChatRole::Assistant)
        .expect("assistant message should persist");
    assert_eq!(cancelled_message.content, "partial ");
    assert_eq!(
        cancelled_message.status,
        kraai_types::MessageStatus::Complete
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn queued_messages_wait_for_tool_batch_completion_before_draining() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain(
            "before tool\n\
<tool_call>\n\
tool: mock_tool\n\
value: alpha\n\
</tool_call>",
        )],
        vec![ScriptedChunk::plain("continuation complete")],
        vec![ScriptedChunk::plain("queued second reply")],
        vec![ScriptedChunk::plain("queued third reply")],
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

    let detection_events = harness
        .events
        .wait_for("tool detection", |events| {
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

    let first_call_id =
        super::harness::call_id_for_queue_order(&detection_events, &session_id, "mock_tool", 0);

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
        .handle
        .send_message(
            session_id.clone(),
            String::from("third message"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    tokio::time::sleep(Duration::from_millis(100)).await;

    harness
        .handle
        .approve_tool(session_id.clone(), first_call_id)
        .await?;
    harness
        .handle
        .execute_approved_tools(session_id.clone())
        .await?;

    harness
        .events
        .wait_for("drain queued messages", |events| {
            stream_complete_count(events, &session_id) >= 4
        })
        .await;

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(
        history
            .values()
            .any(|message| message.content == "continuation complete")
    );
    assert!(
        history
            .values()
            .any(|message| message.content == "queued second reply")
    );
    assert!(
        history
            .values()
            .any(|message| message.content == "queued third reply")
    );

    harness.shutdown().await;
    Ok(())
}
