use super::*;
use color_eyre::eyre::{ensure, eyre};
use std::time::Duration;

fn text_event() -> ProviderStreamEvent {
    ProviderStreamEvent::TextDelta {
        item_id: "message".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "answer".into(),
    }
}

#[tokio::test]
async fn completion_drains_pending_events_without_waiting_for_transport() -> Result<()> {
    let source = stream::iter([Ok(SseEvent::Done)])
        .chain(stream::pending())
        .boxed();
    let events = tokio::time::timeout(
        Duration::from_secs(1),
        adapt_provider_stream(source, "interrupted", |_, pending| {
            pending.extend([
                text_event(),
                ProviderStreamEvent::Usage(TokenUsage::default()),
            ]);
            Ok(StreamStatus::Complete)
        })
        .collect::<Vec<_>>(),
    )
    .await?
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    ensure!(
        events
            == vec![
                text_event(),
                ProviderStreamEvent::Usage(TokenUsage::default())
            ]
    );
    Ok(())
}

#[tokio::test]
async fn decoder_failure_preserves_emitted_events_and_terminates_once() -> Result<()> {
    let source = stream::iter([Ok(SseEvent::Data(String::new()))])
        .chain(stream::pending())
        .boxed();
    let events = tokio::time::timeout(
        Duration::from_secs(1),
        adapt_provider_stream(source, "interrupted", |_, pending| {
            pending.push_back(text_event());
            Err(eyre!("invalid response"))
        })
        .collect::<Vec<_>>(),
    )
    .await?;
    ensure!(events.len() == 2);
    ensure!(
        events
            .first()
            .is_some_and(|event| event.as_ref().is_ok_and(|event| *event == text_event()))
    );
    ensure!(events.get(1).is_some_and(|event| {
        event
            .as_ref()
            .is_err_and(|error| error.to_string() == "invalid response")
    }));
    Ok(())
}

#[tokio::test]
async fn transport_failure_and_eof_are_reported_once() -> Result<()> {
    for (source, expected) in [
        (stream::empty().boxed(), "interrupted"),
        (
            stream::iter([Err(eyre!("connection lost"))])
                .chain(stream::pending())
                .boxed(),
            "connection lost",
        ),
    ] {
        let events = tokio::time::timeout(
            Duration::from_secs(1),
            adapt_provider_stream(source, "interrupted", |_, _| Ok(StreamStatus::Continue))
                .collect::<Vec<_>>(),
        )
        .await?;
        ensure!(events.len() == 1);
        ensure!(events.first().is_some_and(|event| {
            event
                .as_ref()
                .is_err_and(|error| error.to_string().contains(expected))
        }));
    }
    Ok(())
}
