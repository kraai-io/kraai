use super::*;
use crate::app::{KeyCode, KeyEvent, KeyModifiers, UiMode};

fn key(harness: &mut TestHarness, code: KeyCode) {
    harness
        .app
        .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_text(harness: &mut TestHarness, text: &str) {
    for ch in text.chars() {
        key(harness, KeyCode::Char(ch));
    }
}

fn listed(harness: &TestHarness) -> Vec<&'static str> {
    harness
        .app
        .state
        .command_matches()
        .iter()
        .map(|command| command.name)
        .collect()
}

#[test]
fn tab_writes_the_highlighted_command_then_walks_every_match() {
    let mut harness = test_harness();
    type_text(&mut harness, "/m");
    assert_eq!(listed(&harness), ["model", "mcp"]);
    for (code, input, index) in [
        (KeyCode::Tab, "/model", 0),
        (KeyCode::Tab, "/mcp", 1),
        (KeyCode::Tab, "/model", 0),
        (KeyCode::BackTab, "/mcp", 1),
        (KeyCode::BackTab, "/model", 0),
    ] {
        key(&mut harness, code);
        assert_eq!(harness.app.state.input, input);
        assert_eq!(harness.app.state.input_cursor, input.len());
        assert_eq!(harness.app.state.sheet_index, index);
        assert_eq!(listed(&harness), ["model", "mcp"]);
    }
}

#[test]
fn arrows_walk_the_list_and_write_the_command() {
    let mut harness = test_harness();
    type_text(&mut harness, "/");
    let all = listed(&harness);
    assert_eq!(all.len(), 8);
    for (code, input) in [
        (KeyCode::Down, "/agent"),
        (KeyCode::Up, "/model"),
        (KeyCode::Up, "/quit"),
        (KeyCode::Tab, "/model"),
    ] {
        key(&mut harness, code);
        assert_eq!(harness.app.state.input, input);
        assert_eq!(listed(&harness), all);
    }
}

#[test]
fn editing_after_a_walk_filters_by_the_composer_again() {
    let mut harness = test_harness();
    type_text(&mut harness, "/m");
    key(&mut harness, KeyCode::Tab);
    key(&mut harness, KeyCode::Tab);
    assert_eq!(harness.app.state.input, "/mcp");
    key(&mut harness, KeyCode::Backspace);
    assert_eq!(harness.app.state.input, "/mc");
    assert_eq!(listed(&harness), ["mcp"]);
    assert_eq!(harness.app.state.sheet_index, 0);

    key(&mut harness, KeyCode::Backspace);
    key(&mut harness, KeyCode::Tab);
    assert_eq!(harness.app.state.input, "/model");
    type_text(&mut harness, "s");
    assert_eq!(harness.app.state.input, "/models");
    assert!(listed(&harness).is_empty());
}

#[test]
fn enter_runs_the_highlighted_command() {
    let mut harness = test_harness();
    type_text(&mut harness, "/mo");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Model);
    assert!(harness.app.state.input.is_empty());

    let mut harness = test_harness();
    type_text(&mut harness, "/s");
    assert_eq!(listed(&harness), ["sessions", "providers"]);
    key(&mut harness, KeyCode::Tab);
    key(&mut harness, KeyCode::Tab);
    assert_eq!(harness.app.state.input, "/providers");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::ProvidersMenu);
    assert!(harness.app.state.input.is_empty());
    assert!(harness.app.state.command_filter.is_none());
}

#[test]
fn space_runs_a_written_command_that_opens_a_list() {
    let mut harness = test_harness();
    type_text(&mut harness, "/model ");
    assert_eq!(harness.app.state.mode, UiMode::Model);
    assert!(harness.app.state.input.is_empty());

    let mut harness = test_harness();
    type_text(&mut harness, "/mo ");
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(harness.app.state.input, "/mo ");
}

#[test]
fn space_after_a_command_that_takes_arguments_types_a_space() {
    for command in ["/mcp", "/quit", "/new", "/undo"] {
        let mut harness = test_harness();
        type_text(&mut harness, command);
        key(&mut harness, KeyCode::Tab);
        key(&mut harness, KeyCode::Char(' '));
        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert_eq!(harness.app.state.input, format!("{command} "));
        assert!(!harness.app.state.exit);
        assert!(harness.drain_requests().is_empty());
    }
}

#[test]
fn escape_dismisses_the_list_and_keeps_the_written_command() {
    let mut harness = test_harness();
    type_text(&mut harness, "/m");
    key(&mut harness, KeyCode::Tab);
    key(&mut harness, KeyCode::Esc);
    assert!(listed(&harness).is_empty());
    assert_eq!(harness.app.state.input, "/model");
    assert!(harness.app.state.command_filter.is_none());
    key(&mut harness, KeyCode::Tab);
    assert_eq!(harness.app.state.input, "/model");
}

#[test]
fn error_is_listed_and_opens_only_while_an_error_waits() {
    let mut harness = test_harness();
    type_text(&mut harness, "/err");
    assert!(listed(&harness).is_empty());
    type_text(&mut harness, "or");
    key(&mut harness, KeyCode::Enter);
    assert!(!harness.app.state.error_open);
    assert_eq!(harness.app.state.status, "No error to read");
    assert!(harness.app.state.input.is_empty());

    harness.app.set_error(String::from("Stream error: offline"));
    type_text(&mut harness, "/err");
    assert_eq!(listed(&harness), ["error"]);
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.error_open);
    assert!(harness.app.state.input.is_empty());
    key(&mut harness, KeyCode::Esc);
    assert!(!harness.app.state.error_open);
    assert!(harness.app.state.last_error.is_some());
}
