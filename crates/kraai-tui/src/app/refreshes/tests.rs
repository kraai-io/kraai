use kraai_runtime::Event;

use super::*;

fn snapshot_request(session: &str) -> RuntimeRequest {
    RuntimeRequest::GetSessionSnapshot {
        session_id: SessionId::new(session),
    }
}

fn session_event(session: &str, sequence: u64) -> RuntimeEvent {
    RuntimeEvent {
        sequence,
        event: Event::HistoryUpdated {
            session_id: SessionId::new(session),
        },
    }
}

#[test]
fn list_refreshes_keep_one_request_in_flight_and_repeat_once() {
    let mut refreshes = Refreshes::default();
    assert!(refreshes.start(&RuntimeRequest::ListSessions));
    assert!(!refreshes.start(&RuntimeRequest::ListSessions));
    assert!(!refreshes.start(&RuntimeRequest::ListSessions));
    assert!(refreshes.start(&RuntimeRequest::ListUserInputHistory { limit: 1 }));
    assert!(refreshes.start(&RuntimeRequest::ListModels));
    assert!(refreshes.start(&RuntimeRequest::ListModels));

    assert!(matches!(
        refreshes.finish(&RuntimeResponse::Sessions(Ok(Vec::new()))),
        FinishedRefresh::Repeat(RuntimeRequest::ListSessions)
    ));
    assert!(matches!(
        refreshes.finish(&RuntimeResponse::UserInputHistory(Ok(Vec::new()))),
        FinishedRefresh::None
    ));
    assert!(refreshes.start(&RuntimeRequest::ListSessions));
    assert!(matches!(
        refreshes.finish(&RuntimeResponse::Sessions(Ok(Vec::new()))),
        FinishedRefresh::None
    ));
}

#[test]
fn snapshots_buffer_their_session_until_they_land() {
    let mut refreshes = Refreshes::default();
    assert!(refreshes.buffer(session_event("a", 1)).is_some());
    assert!(refreshes.start(&snapshot_request("a")));
    assert!(refreshes.start(&snapshot_request("b")));
    assert!(!refreshes.start(&snapshot_request("a")));
    assert!(refreshes.buffer(session_event("a", 2)).is_none());
    assert!(refreshes.buffer(session_event("a", 3)).is_none());
    assert!(refreshes.buffer(session_event("c", 4)).is_some());
    assert!(
        refreshes
            .buffer(RuntimeEvent {
                sequence: 5,
                event: Event::ModelsUpdated,
            })
            .is_some()
    );

    let landed = refreshes.finish(&RuntimeResponse::SessionSnapshot {
        session_id: SessionId::new("a"),
        model_save_id: 0,
        result: Box::new(Err(kraai_runtime::RuntimeError::unavailable("offline"))),
    });
    assert!(matches!(
        landed,
        FinishedRefresh::Snapshot { session_id, dirty: true, buffered }
            if session_id == SessionId::new("a")
                && buffered.iter().map(|event| event.sequence).eq([2, 3])
    ));
    assert!(refreshes.buffer(session_event("a", 6)).is_some());
    assert!(!refreshes.start(&snapshot_request("b")));
}
