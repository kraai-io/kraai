use super::*;
use crate::app::{KeyCode, KeyEvent, KeyModifiers, UiMode};

#[test]
fn page_keys_follow_viewport_size_in_chat_and_executions() {
    for mode in [UiMode::Chat, UiMode::Executions] {
        for height in [0, 1, 2, 20, 50] {
            let mut harness = test_harness();
            harness.app.state.mode = mode.clone();
            harness.app.state.chat_render_cache.borrow_mut().total_lines = 200;
            harness.app.state.chat_viewport_height = height;
            let bottom = 200 - height;
            let step = height.saturating_sub(2).max(1);

            harness
                .app
                .handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
            assert_eq!(harness.app.state.scroll, bottom - step);
            assert!(!harness.app.state.auto_scroll);
            harness
                .app
                .handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
            assert_eq!(harness.app.state.scroll, bottom);
            assert!(harness.app.state.auto_scroll);
        }
    }
}

#[test]
fn pending_input_leaves_runtime_notifications_queued() {
    let mut harness = test_harness();
    let (tx, rx) = unbounded();
    harness.app.runtime_rx = rx;
    assert!(
        tx.send(RuntimeResponse::OpenAiCodexAuthStatus(Ok(
            Default::default()
        )))
        .is_ok()
    );
    assert!(matches!(harness.app.process_events(|| Ok(true)), Ok(false)));
    assert_eq!(harness.app.runtime_rx.len(), 1);
    assert!(matches!(harness.app.process_events(|| Ok(false)), Ok(true)));
    assert!(harness.app.runtime_rx.is_empty());
}

#[test]
fn runtime_backlog_yields_before_draining_the_queue() {
    let mut harness = test_harness();
    let (tx, rx) = unbounded();
    harness.app.runtime_rx = rx;
    for _ in 0..200 {
        assert!(
            tx.send(RuntimeResponse::OpenAiCodexAuthStatus(Ok(
                Default::default()
            )))
            .is_ok()
        );
    }
    assert!(matches!(harness.app.process_events(|| Ok(false)), Ok(true)));
    assert!(harness.app.runtime_rx.len() >= 136);
}

#[test]
fn input_arriving_during_runtime_processing_stops_the_batch() {
    let mut harness = test_harness();
    let (tx, rx) = unbounded();
    harness.app.runtime_rx = rx;
    for _ in 0..3 {
        assert!(
            tx.send(RuntimeResponse::OpenAiCodexAuthStatus(Ok(
                Default::default()
            )))
            .is_ok()
        );
    }
    let mut polls = 0;
    assert!(matches!(
        harness.app.process_events(|| {
            polls += 1;
            Ok(polls >= 3)
        }),
        Ok(true)
    ));
    assert_eq!(harness.app.runtime_rx.len(), 2);
}
