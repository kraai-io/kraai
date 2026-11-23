use super::*;

#[test]
fn scoped_subscribers_are_isolated_and_receive_service_failures() {
    let sender = RuntimeEventSender::new(2);
    let mut first = sender.subscribe_session(&SessionId::new("first"));
    let mut second = sender.subscribe_session(&SessionId::new("second"));
    for _ in 0..1025 {
        sender.send(Event::ItemDelta {
            session_id: SessionId::new("second"),
            message_id: "message".into(),
            item_index: 0,
            delta: "text".into(),
        });
    }
    assert!(matches!(
        first.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        second.try_recv(),
        Err(broadcast::error::TryRecvError::Lagged(_))
    ));
    sender.send(Event::ServiceError {
        error: crate::RuntimeError::unavailable("service stopped"),
    });
    assert!(matches!(
        first.try_recv().unwrap().event,
        Event::ServiceError { .. }
    ));
}

#[test]
fn scoped_channels_preserve_order_and_release_detached_sessions() {
    let sender = RuntimeEventSender::new(4);
    let mut first = sender.subscribe_session(&SessionId::new("first"));
    let mut global = sender.subscribe();
    sender.send(Event::ItemDelta {
        session_id: SessionId::new("first"),
        message_id: "message".into(),
        item_index: 0,
        delta: "text".into(),
    });
    assert_eq!(
        first.try_recv().unwrap().sequence,
        global.try_recv().unwrap().sequence
    );
    drop(first);
    let _second = sender.subscribe_session(&SessionId::new("second"));
    let state = sender
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(!state.sessions.contains_key(&SessionId::new("first")));
    assert!(state.sessions.contains_key(&SessionId::new("second")));
    drop(state);
}

#[test]
fn terminal_events_do_not_finish_a_turn_before_cleanup() {
    let sender = RuntimeEventSender::new(16);
    sender.resume_timer(&SessionId::new("session"));
    let timer = sender.turn_snapshot(&SessionId::new("session")).timer;
    for event in [
        Event::StreamError {
            session_id: SessionId::new("session"),
            message_id: "message".into(),
            error: "rollback failed".into(),
        },
        Event::ContinuationFailed {
            session_id: SessionId::new("session"),
            error: "rollback failed".into(),
        },
        Event::StreamCancelled {
            session_id: SessionId::new("session"),
            message_id: "message".into(),
        },
    ] {
        sender.send(event);
        assert_eq!(
            sender.turn_snapshot(&SessionId::new("session")).timer,
            timer
        );
    }
    sender.finish_timer(&SessionId::new("session"));
    assert!(
        sender
            .turn_snapshot(&SessionId::new("session"))
            .timer
            .elapsed(std::time::Instant::now())
            .is_none()
    );
}

#[test]
fn turn_outcome_follows_the_latest_turn() {
    let sender = RuntimeEventSender::new(16);
    let session = SessionId::new("session");
    let outcome = || sender.turn_snapshot(&session).outcome;
    let start = || Event::StreamStart {
        session_id: session.clone(),
        message_id: "message".into(),
    };
    sender.send(Event::StreamError {
        session_id: session.clone(),
        message_id: "message".into(),
        error: "rate limited".into(),
    });
    assert!(matches!(
        outcome(),
        Some(crate::TurnOutcome::Failed { error }) if error.message == "rate limited"
    ));
    sender.send(start());
    assert_eq!(outcome(), None);
    sender.send(Event::TurnCompleted {
        session_id: session.clone(),
    });
    assert_eq!(outcome(), Some(crate::TurnOutcome::Completed));
    sender.send(start());
    sender.send(Event::StreamCancelled {
        session_id: session.clone(),
        message_id: "message".into(),
    });
    assert_eq!(outcome(), Some(crate::TurnOutcome::Cancelled));
    sender.clear_outcome(&session);
    assert_eq!(outcome(), None);
    sender.send(Event::TurnCompleted {
        session_id: session.clone(),
    });
    sender.forget_session(&session);
    assert_eq!(outcome(), None);
}

#[test]
fn session_timing_survives_detaching_and_other_session_activity() {
    let sender = RuntimeEventSender::new(16);
    let receiver = sender.subscribe();
    sender.send(Event::StreamStart {
        session_id: SessionId::new("first"),
        message_id: "message".into(),
    });
    let first = sender.turn_snapshot(&SessionId::new("first")).timer;
    drop(receiver);
    sender.send(Event::StreamStart {
        session_id: SessionId::new("second"),
        message_id: "other".into(),
    });
    sender.send(Event::StreamCancelled {
        session_id: SessionId::new("second"),
        message_id: "other".into(),
    });
    let _receiver = sender.subscribe();
    assert_eq!(sender.turn_snapshot(&SessionId::new("first")).timer, first);
    assert!(first.elapsed(std::time::Instant::now()).is_some());
    sender.finish_timer(&SessionId::new("first"));
    let finished = sender.turn_snapshot(&SessionId::new("first")).timer;
    assert!(finished.elapsed(std::time::Instant::now()).is_none());
    assert!(finished.last_duration().is_some());
}

#[test]
fn concurrent_producers_publish_in_sequence_order() {
    const PRODUCERS: usize = 8;
    const EVENTS_PER_PRODUCER: usize = 10_000;
    let sender = RuntimeEventSender::new(PRODUCERS * EVENTS_PER_PRODUCER);
    let mut receiver = sender.subscribe();
    let start = std::sync::Barrier::new(PRODUCERS);
    std::thread::scope(|scope| {
        for _ in 0..PRODUCERS {
            let sender = sender.clone();
            let start = &start;
            scope.spawn(move || {
                start.wait();
                for _ in 0..EVENTS_PER_PRODUCER {
                    sender.send(Event::ConfigLoaded);
                }
            });
        }
    });
    for expected in 1..=(PRODUCERS * EVENTS_PER_PRODUCER) as u64 {
        assert_eq!(
            receiver.try_recv().map(|event| event.sequence),
            Ok(expected)
        );
    }
    assert_eq!(
        sender.latest_sequence(),
        (PRODUCERS * EVENTS_PER_PRODUCER) as u64
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}
