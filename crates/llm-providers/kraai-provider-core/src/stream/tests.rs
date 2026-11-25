use super::*;
use color_eyre::eyre::eyre;

fn text_event() -> ProviderStreamEvent {
    ProviderStreamEvent::TextDelta {
        item_id: "message".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "answer".into(),
    }
}

#[tokio::test]
async fn completion_drains_pending_events_without_waiting_for_transport() {
    let source = stream::iter([Ok(SseEvent::Done)])
        .chain(stream::pending())
        .boxed();
    let events = kraai_test_support::hang_guard(
        adapt_provider_stream(source, "interrupted", |_, pending| {
            pending.extend([
                text_event(),
                ProviderStreamEvent::Usage(TokenUsage::default()),
            ]);
            Ok(StreamStatus::Complete)
        })
        .collect::<Vec<_>>(),
    )
    .await
    .unwrap()
    .into_iter()
    .collect::<Result<Vec<_>>>()
    .unwrap();
    assert!(
        events
            == vec![
                text_event(),
                ProviderStreamEvent::Usage(TokenUsage::default())
            ]
    );
}

#[tokio::test]
async fn decoder_failure_preserves_emitted_events_and_terminates_once() {
    let source = stream::iter([Ok(SseEvent::Data(String::new()))])
        .chain(stream::pending())
        .boxed();
    let events = kraai_test_support::hang_guard(
        adapt_provider_stream(source, "interrupted", |_, pending| {
            pending.push_back(text_event());
            Err(eyre!("invalid response"))
        })
        .collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    assert!(events.len() == 2);
    assert!(
        events
            .first()
            .is_some_and(|event| event.as_ref().is_ok_and(|event| *event == text_event()))
    );
    assert!(events.get(1).is_some_and(|event| {
        event
            .as_ref()
            .is_err_and(|error| error.to_string() == "invalid response")
    }));
}

#[tokio::test]
async fn transport_failure_and_eof_are_reported_once() {
    for (source, expected) in [
        (stream::empty().boxed(), "interrupted"),
        (
            stream::iter([Err(eyre!("connection lost"))])
                .chain(stream::pending())
                .boxed(),
            "connection lost",
        ),
    ] {
        let events = kraai_test_support::hang_guard(
            adapt_provider_stream(source, "interrupted", |_, _| Ok(StreamStatus::Continue))
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
        assert!(events.len() == 1);
        assert!(events.first().is_some_and(|event| {
            event
                .as_ref()
                .is_err_and(|error| error.to_string().contains(expected))
        }));
    }
}

#[tokio::test]
async fn decoder_can_complete_at_eof() {
    let events = kraai_test_support::hang_guard(
        adapt_provider_stream(stream::empty().boxed(), "interrupted", |event, pending| {
            assert!(event.is_none());
            pending.push_back(text_event());
            Ok(StreamStatus::Complete)
        })
        .collect::<Vec<_>>(),
    )
    .await
    .unwrap()
    .into_iter()
    .collect::<Result<Vec<_>>>()
    .unwrap();
    assert!(events == vec![text_event()]);
}
