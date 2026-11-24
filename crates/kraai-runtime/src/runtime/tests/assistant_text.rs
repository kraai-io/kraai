use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn native_assistant_text_preserves_literal_tool_envelopes() {
    let text = "Example:\n<tool_call>print 'example'</tool_call>\nThis is literal text.";
    let harness = RuntimeTestHarness::new_native(vec![vec![ScriptedChunk::plain(text)]]).await;
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "Explain the envelope".into(),
            "mock-model".into(),
            "mock-native".into(),
            Default::default(),
        )
        .await
        .unwrap();
    let events = harness.events.wait_for("native text completion", |events| {
        events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id: id } if id == &session_id))
    }).await;
    let chunks = events
        .iter()
        .filter_map(|event| super::harness::streamed_text(event, &session_id))
        .collect::<String>();
    assert_eq!(chunks, text);
    assert!(!events.iter().any(
        |event| matches!(event, Event::ScriptPrepared { session_id: id, .. } if id == &session_id)
    ));
    harness.shutdown().await;
}

#[tokio::test]
async fn a_failed_stream_keeps_its_streamed_text_as_cancelled() {
    let harness = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::plain("partial answer"),
        ScriptedChunk::error("provider rejected the stream"),
    ]])
    .await;
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "explain".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await
        .unwrap();
    let events = harness
        .events
        .wait_for("stream error", |events| {
            events.iter().any(|event| matches!(event, Event::StreamError { session_id: id, .. } if id == &session_id))
        })
        .await;
    let failed = super::harness::started_stream(&events, &session_id);
    let snapshot = harness
        .handle
        .get_session_snapshot(session_id.clone())
        .await
        .unwrap();
    let message = snapshot
        .history
        .get(&failed)
        .expect("failed message is kept");
    assert_eq!(message.status, kraai_types::MessageStatus::Cancelled);
    assert_eq!(message.display_text(), "partial answer");
    assert!(!snapshot.session.is_running);
    harness.shutdown().await;
}
