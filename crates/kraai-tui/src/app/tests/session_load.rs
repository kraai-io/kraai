#![expect(
    clippy::expect_used,
    reason = "test setup requires a session load request"
)]

use super::{RuntimeRequest, RuntimeResponse, TestHarness, session_snapshot, test_harness};
use crate::app::{KeyCode, KeyEvent, KeyModifiers, UiMode};
use crossbeam_channel::unbounded;

fn begin_load(harness: &mut TestHarness) -> (u64, String) {
    harness.app.state.sessions = vec![session_snapshot(None).session];
    harness.app.state.mode = UiMode::SessionsMenu;
    harness.app.state.sessions_menu_index = 1;
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    harness
        .drain_requests()
        .into_iter()
        .find_map(|request| match request {
            RuntimeRequest::LoadSession {
                load_id,
                session_id,
            } => Some((load_id, session_id)),
            _ => None,
        })
        .expect("session selection must request a load")
}

#[test]
fn new_chat_ignores_late_load_success_and_failure() {
    for result in [
        Ok(true),
        Ok(false),
        Err(kraai_runtime::RuntimeError::unavailable("load failed")),
    ] {
        let mut harness = test_harness();
        let (_event_tx, event_rx) = unbounded();
        let (response_tx, response_rx) = unbounded();
        harness.app.event_rx = event_rx;
        harness.app.runtime_rx = response_rx;
        let (load_id, session_id) = begin_load(&mut harness);
        harness
            .app
            .handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        harness
            .app
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(harness.app.state.current_session_id.is_none());
        harness
            .app
            .set_input_text(String::from("new conversation draft"));
        assert!(
            response_tx
                .send(RuntimeResponse::LoadSession {
                    load_id,
                    session_id,
                    result
                })
                .is_ok()
        );

        assert!(harness.app.process_events());

        assert!(harness.app.state.current_session_id.is_none());
        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert_eq!(harness.app.state.input, "new conversation draft");
        assert_eq!(harness.app.state.status, "Started new chat");
        assert!(harness.app.state.last_error.is_none());
    }
}

#[test]
fn current_load_success_still_opens_and_synchronizes_the_session() {
    let mut harness = test_harness();
    let (load_id, session_id) = begin_load(&mut harness);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::LoadSession {
            load_id,
            session_id: session_id.clone(),
            result: Ok(true),
        });

    assert_eq!(
        harness.app.state.current_session_id,
        Some(session_id.clone())
    );
    assert_eq!(harness.app.state.status, "Session loaded");
    assert!(harness.drain_requests().iter().any(|request| matches!(
        request,
        RuntimeRequest::GetSessionSnapshot { session_id: requested } if requested == &session_id
    )));
}

#[test]
fn current_load_failure_keeps_existing_error_messages() {
    for (result, status) in [
        (Ok(false), "Session not found"),
        (
            Err(kraai_runtime::RuntimeError::unavailable("load failed")),
            "Failed to load session: load failed",
        ),
    ] {
        let mut harness = test_harness();
        let (load_id, session_id) = begin_load(&mut harness);
        harness
            .app
            .handle_runtime_response(RuntimeResponse::LoadSession {
                load_id,
                session_id,
                result,
            });
        assert!(harness.app.state.current_session_id.is_none());
        assert_eq!(harness.app.state.status, status);
        assert!(harness.app.state.pending_session_load_id.is_none());
    }
}

#[test]
fn only_the_latest_load_can_change_navigation_when_request_ids_wrap() {
    let mut harness = test_harness();
    harness.app.state.next_session_load_id = u64::MAX;
    let (previous_id, previous_session) = begin_load(&mut harness);
    let (current_id, current_session) = begin_load(&mut harness);
    assert_eq!(previous_id, u64::MAX);
    assert_eq!(current_id, 0);
    for result in [
        Ok(true),
        Ok(false),
        Err(kraai_runtime::RuntimeError::unavailable("old load failed")),
    ] {
        harness
            .app
            .handle_runtime_response(RuntimeResponse::LoadSession {
                load_id: previous_id,
                session_id: previous_session.clone(),
                result,
            });
        assert!(harness.app.state.current_session_id.is_none());
        assert!(harness.app.state.last_error.is_none());
        assert_eq!(harness.app.state.mode, UiMode::SessionsMenu);
        assert_eq!(harness.app.state.pending_session_load_id, Some(current_id));
    }
    harness
        .app
        .handle_runtime_response(RuntimeResponse::LoadSession {
            load_id: current_id,
            session_id: current_session.clone(),
            result: Ok(true),
        });
    assert_eq!(harness.app.state.current_session_id, Some(current_session));
    assert!(harness.app.state.pending_session_load_id.is_none());
}
