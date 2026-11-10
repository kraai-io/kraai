use super::*;

#[test]
fn rejected_values_show_the_error_in_the_sheet_and_keep_the_text() -> color_eyre::Result<()> {
    for (typed, expected) in [
        ("unset", "cannot be cleared"),
        ("invalid", "Effort has no choice named invalid"),
    ] {
        let mut harness = test_harness();
        configure(&mut harness)?;
        open_settings(&mut harness, "effort")?;
        harness.drain_requests();
        type_text(&mut harness, typed);
        key(&mut harness, KeyCode::Enter);
        assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
        assert_eq!(harness.app.state.sheet_query, typed);
        assert!(harness.drain_requests().is_empty());
        assert!(harness.app.state.selected_model_options.is_empty());
        assert!(
            harness
                .app
                .state
                .sheet_notice
                .as_ref()
                .is_some_and(|notice| notice.contains(expected)),
            "{:?}",
            harness.app.state.sheet_notice
        );
        assert!(harness.app.state.last_error.is_none());
        let rendered = screen(&harness.app.state, 80, 20);
        assert!(rendered.contains(expected));
        assert!(rendered.contains(&format!("/model › Model › {typed}")));
        key(&mut harness, KeyCode::Backspace);
        assert!(harness.app.state.sheet_notice.is_none());
    }
    Ok(())
}

#[test]
fn typed_text_must_name_a_listed_choice_even_when_the_option_is_bound_to_the_request()
-> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    model_mut(&mut harness)?
        .options
        .iter_mut()
        .find(|option| option.id == "effort")
        .ok_or_else(|| color_eyre::eyre::eyre!("missing effort option"))?
        .binding = serde_json::from_value(serde_json::json!({"type":"body","path":"/effort"}))?;
    open_settings(&mut harness, "effort")?;
    type_text(&mut harness, "7");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        harness.app.state.sheet_notice.as_deref(),
        Some("Effort has no choice named 7")
    );
    assert_eq!(harness.app.state.sheet_query, "7");
    assert_eq!(value(&harness, "effort"), None);
    key(&mut harness, KeyCode::Backspace);
    type_text(&mut harness, "HIGH");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
    Ok(())
}

#[test]
fn out_of_range_number_shows_the_bounds_in_the_sheet_and_keeps_the_text() -> color_eyre::Result<()>
{
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    open_settings(&mut harness, "budget")?;
    type_text(&mut harness, "101");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    assert_eq!(harness.app.state.sheet_query, "101");
    assert_eq!(
        value(&harness, "budget"),
        Some(&ModelOptionValue::Integer(50))
    );
    assert_eq!(
        harness.app.state.sheet_notice.as_deref(),
        Some("budget: Budget is outside its supported bounds")
    );
    for (width, height) in [(80, 20), (40, 12), (30, 10)] {
        assert!(screen(&harness.app.state, width, height).contains("bounds"));
    }
    harness.app.set_error("Unrelated runtime error".into());
    key(&mut harness, KeyCode::Backspace);
    assert!(harness.app.state.sheet_notice.is_none());
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        value(&harness, "budget"),
        Some(&ModelOptionValue::Integer(10))
    );
    assert_eq!(
        harness.app.state.last_error.as_deref(),
        Some("Unrelated runtime error")
    );
    Ok(())
}

#[test]
fn reopening_the_sheet_clears_the_previous_notice_and_text() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    open_settings(&mut harness, "budget")?;
    type_text(&mut harness, "-");
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.sheet_notice.is_some());
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Model);
    assert!(harness.app.state.sheet_notice.is_none());
    assert!(harness.app.state.sheet_query.is_empty());
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    assert!(harness.app.state.sheet_notice.is_none());
    assert!(harness.app.state.sheet_query.is_empty());
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
    assert!(screen(&harness.app.state, 80, 20).contains("Choose model options in /model"));
    harness.app.open_error();
    let rendered = screen(&harness.app.state, 80, 20);
    assert!(rendered.contains("effort"));
    assert!(rendered.contains("Choose Thinking"));
    assert!(harness.app.state.error_open);
    Ok(())
}
