use super::{Event, test_harness};

#[test]
fn host_failure_is_visible_in_the_tui() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.state.is_streaming = true;
    let error = "Incompatible Nushell sibling; rebuild both binaries with `just build`";
    harness.app.handle_runtime_event(Event::ContinuationFailed {
        session_id: String::from("session"),
        error: String::from(error),
    });
    assert!(!harness.app.state.is_streaming);
    assert!(
        harness
            .app
            .state
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains(error))
    );
}
