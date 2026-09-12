use super::*;
use crate::app::{CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};

#[test]
fn editor_request_stops_buffered_input_from_submitting_the_draft() {
    let mut harness = test_harness();
    harness.app.set_input_text(String::from("unfinished draft"));
    harness
        .app
        .handle_terminal_event(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        )));
    assert!(
        !harness
            .app
            .handle_terminal_event(CrosstermEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
    );
    assert_eq!(harness.app.state.input, "unfinished draft");
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn edited_prompt_does_not_overwrite_an_undo_received_while_editing() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.set_input_text(String::from("draft"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::UndoLastUserMessage {
            session_id: String::from("session"),
            result: Ok(Some(String::from("restored message"))),
        });
    assert!(
        harness
            .app
            .apply_edited_prompt(String::from("edited draft"), Some("session"), "draft")
            .is_err()
    );
    assert_eq!(harness.app.state.input, "restored message");
}

#[test]
fn edited_prompt_does_not_move_into_another_session() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("other-session"));
    harness.app.set_input_text(String::from("draft"));
    assert!(
        harness
            .app
            .apply_edited_prompt(String::from("edited draft"), Some("session"), "draft")
            .is_err()
    );
    assert_eq!(harness.app.state.input, "draft");
}

#[test]
fn edited_prompt_updates_unchanged_session_and_draft() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.set_input_text(String::from("draft"));
    assert!(
        harness
            .app
            .apply_edited_prompt(String::from("edited draft"), Some("session"), "draft")
            .is_ok()
    );
    assert_eq!(harness.app.state.input, "edited draft");
}

#[test]
fn edited_prompt_is_preserved_when_runtime_exit_arrives_during_editing() {
    let mut harness = test_harness();
    harness.app.set_input_text(String::from("draft"));
    harness.app.state.exit = true;
    assert!(
        harness
            .app
            .apply_edited_prompt(String::from("edited draft"), None, "draft")
            .is_err()
    );
    assert_eq!(harness.app.state.input, "draft");
}
