use color_eyre::eyre::Result;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn native_assistant_text_preserves_literal_tool_envelopes() -> Result<()> {
    let text = "Example:\n<tool_call>print 'example'</tool_call>\nThis is literal text.";
    let Some(harness) =
        RuntimeTestHarness::new_native(vec![vec![ScriptedChunk::plain(text)]]).await
    else {
        return Ok(());
    };
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "Explain the envelope".into(),
            "mock-model".into(),
            "mock-native".into(),
        )
        .await?;
    let events = harness.events.wait_for("native text completion", |events| {
        events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id: id } if id == &session_id))
    }).await;
    let chunks = events
        .iter()
        .filter_map(|event| match event {
            Event::StreamChunk {
                session_id: id,
                chunk,
                ..
            } if id == &session_id => Some(chunk.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(chunks, text);
    assert!(!events.iter().any(
        |event| matches!(event, Event::ScriptPrepared { session_id: id, .. } if id == &session_id)
    ));
    harness.shutdown().await;
    Ok(())
}
