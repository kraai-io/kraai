use super::*;
use crate::app::UiMode;
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

fn screen(state: &AppState, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    state.render(area, &mut buffer);
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn invalid_option_commands_show_errors_and_preserve_the_draft() -> color_eyre::Result<()> {
    for (command, expected) in [
        ("/option unknown high", "Unknown model option"),
        ("/option effort --clear", "cannot be cleared"),
        ("/option effort invalid", "Unsupported choice"),
        ("/option effort", "Usage: /option"),
    ] {
        let mut harness = test_harness();
        configure(&mut harness)?;
        harness.app.set_input_text(command.into());
        harness.app.handle_submit();
        assert_eq!(harness.app.state.input, command);
        assert!(harness.drain_requests().is_empty());
        assert!(
            harness
                .app
                .state
                .last_error
                .as_ref()
                .is_some_and(|error| error.contains(expected)),
            "{:?}",
            harness.app.state.last_error
        );
        assert!(screen(&harness.app.state, 80, 20).contains(expected));
        harness
            .app
            .handle_key_event(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        assert!(harness.app.state.error_open);
        assert!(screen(&harness.app.state, 80, 20).contains(expected));
        harness
            .app
            .handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(harness.app.state.last_error.is_none());
        assert_eq!(harness.app.state.input, command);
    }
    Ok(())
}

#[test]
fn integer_editor_shows_validation_inside_popup_and_keeps_invalid_input() -> color_eyre::Result<()>
{
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    edit(&mut harness, "budget")?;
    harness.app.state.option_editor_input = "101".into();
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.option_editor.is_some());
    assert_eq!(harness.app.state.option_editor_input, "101");
    assert_eq!(
        harness.app.state.selected_model_options.get("budget"),
        Some(&ModelOptionValue::Integer(50))
    );
    assert_eq!(
        harness.app.state.option_editor_error.as_deref(),
        Some("budget: Budget is outside its supported bounds")
    );
    for (width, height) in [(80, 20), (40, 12), (30, 10)] {
        assert!(screen(&harness.app.state, width, height).contains("bounds"));
    }
    harness.app.set_error("Unrelated runtime error".into());
    key(&mut harness, KeyCode::Backspace);
    assert!(harness.app.state.option_editor_error.is_none());
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.option_editor.is_none());
    assert_eq!(
        harness.app.state.selected_model_options.get("budget"),
        Some(&ModelOptionValue::Integer(10))
    );
    assert_eq!(
        harness.app.state.last_error.as_deref(),
        Some("Unrelated runtime error")
    );
    Ok(())
}

#[test]
fn reopening_options_clears_previous_editor_feedback() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    edit(&mut harness, "budget")?;
    harness.app.state.option_editor_input = "-".into();
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.option_editor_error.is_some());
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    harness.app.open_model_options();
    assert!(harness.app.state.option_editor_error.is_none());
    assert!(harness.app.state.option_editor.is_none());
    Ok(())
}

#[test]
fn missing_required_options_block_sending_with_visible_feedback() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness.app.set_input_text("Hello".into());
    harness.app.handle_submit();
    assert!(harness.drain_requests().is_empty());
    assert_eq!(harness.app.state.input, "Hello");
    assert!(!harness.app.state.exit);
    assert!(screen(&harness.app.state, 80, 20).contains("Choose model options using /option"));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
    let rendered = screen(&harness.app.state, 80, 20);
    assert!(rendered.contains("effort"));
    assert!(rendered.contains("thinking"));
    assert!(harness.app.state.error_open);
    Ok(())
}
