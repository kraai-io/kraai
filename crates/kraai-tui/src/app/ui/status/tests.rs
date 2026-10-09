use super::*;

#[test]
fn footer_activity_has_one_space_left_inset() {
    for is_streaming in [false, true] {
        let state = AppState {
            is_streaming,
            ..AppState::default()
        };
        let area = Rect::new(3, 4, 80, 2);
        let mut buffer = Buffer::empty(area);
        render_status(&state, area, &mut buffer);
        assert_eq!(
            buffer[(area.x + 1, area.y + 1)].symbol(),
            if is_streaming { "·" } else { "R" }
        );
        assert_eq!(buffer[(area.x, area.y + 1)].symbol(), " ");
        assert_eq!(buffer[(area.right() - 1, area.y + 1)].symbol(), " ");
    }
}

#[test]
fn footer_layout_leaves_a_blank_row_above_status() {
    for last_error in [None, Some(String::from("error"))] {
        let state = AppState {
            last_error,
            ..AppState::default()
        };
        let area = Rect::new(0, 0, 80, 24);
        let [history, footer, input] = super::super::chat_layout(&state, area);
        assert_eq!(history.bottom(), footer.y);
        assert_eq!(footer.bottom(), input.y);
        assert_eq!(footer.height, 2 + u16::from(state.last_error.is_some()));
        let mut buffer = Buffer::empty(footer);
        render_status(&state, footer, &mut buffer);
        let rows: Vec<String> = buffer
            .content()
            .chunks(usize::from(footer.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        assert!(rows.first().is_some_and(|row| row.trim().is_empty()));
        assert!(rows.last().is_some_and(|row| !row.trim().is_empty()));
        if state.last_error.is_some() {
            assert!(rows.get(1).is_some_and(|row| row.contains("F8 error")));
        }
    }
}

#[test]
fn footer_starts_with_activity_and_keeps_errors_accessible() {
    let mut state = AppState {
        selected_provider_id: Some(String::from("provider")),
        selected_model_id: Some(String::from("Astra")),
        ..AppState::default()
    };
    let area = Rect::new(0, 0, 100, 1);
    let mut buffer = Buffer::empty(area);
    render_status(&state, area, &mut buffer);
    let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
    assert_eq!(text.trim(), "Ready · Astra · 0 · $0.0000");
    state.last_error = Some("A long error ".repeat(30));
    for width in [28, 40, 80, 120] {
        let area = Rect::new(0, 0, width, 2);
        let mut buffer = Buffer::empty(area);
        render_status(&state, area, &mut buffer);
        let rows: Vec<String> = buffer
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        assert!(rows.first().is_some_and(|row| row.contains("F8 error")));
        assert!(
            rows.get(1)
                .is_some_and(|row| row.contains("Astra") && !row.contains("F8 error"))
        );
    }
}

#[test]
fn footer_compacts_context_on_narrow_terminals() {
    let state = AppState {
        selected_provider_id: Some(String::from("provider")),
        selected_model_id: Some(String::from("Astra")),
        context_usage: Some(kraai_runtime::SessionContextUsage {
            provider_id: String::from("provider"),
            model_id: String::from("Astra"),
            max_context: Some(272_000),
            usage: kraai_types::TokenUsage {
                input_tokens: 136_000,
                ..Default::default()
            },
        }),
        ..AppState::default()
    };
    for (width, expected) in [
        (40, "50% · $0.0000"),
        (100, "136,000/272,000 (50%) · $0.0000"),
    ] {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
        render_status(&state, buffer.area, &mut buffer);
        let row: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(row.trim_end().ends_with(expected), "{row}");
    }
}

#[test]
fn context_limit_follows_model_and_provider_selection() {
    let mut state = AppState {
        selected_provider_id: Some(String::from("first")),
        selected_model_id: Some(String::from("model")),
        models_by_provider: std::collections::HashMap::from([
            (
                String::from("first"),
                vec![
                    kraai_runtime::Model {
                        id: String::from("model"),
                        name: String::from("Model"),
                        max_context: Some(100_000),
                        options: Vec::new(),
                    },
                    kraai_runtime::Model {
                        id: String::from("other"),
                        name: String::from("Other"),
                        max_context: Some(200_000),
                        options: Vec::new(),
                    },
                ],
            ),
            (
                String::from("second"),
                vec![kraai_runtime::Model {
                    id: String::from("model"),
                    name: String::from("Model"),
                    max_context: Some(50_000),
                    options: Vec::new(),
                }],
            ),
        ]),
        context_usage: Some(kraai_runtime::SessionContextUsage {
            provider_id: String::from("first"),
            model_id: String::from("model"),
            max_context: Some(100_000),
            usage: kraai_types::TokenUsage {
                input_tokens: 20_000,
                ..Default::default()
            },
        }),
        ..AppState::default()
    };
    for (provider, model, expected) in [
        ("first", "model", "20,000/100,000 (20%)"),
        ("first", "other", "20,000/200,000 (10%)"),
        ("second", "model", "20,000/50,000 (40%)"),
        ("second", "unknown", "20,000"),
        ("first", "model", "20,000/100,000 (20%)"),
    ] {
        state.selected_provider_id = Some(provider.into());
        state.selected_model_id = Some(model.into());
        assert_eq!(statusline_context_label(&state), expected);
    }
    for (limit, expected) in [(Some(400_000), "20,000/400,000 (5%)"), (None, "20,000")] {
        for model in state.models_by_provider.values_mut().flatten() {
            model.max_context = limit;
        }
        assert_eq!(statusline_context_label(&state), expected);
    }
}

#[test]
fn undiscovered_model_uses_only_its_own_recorded_context_limit() {
    let mut state = AppState {
        context_usage: Some(kraai_runtime::SessionContextUsage {
            provider_id: String::from("provider"),
            model_id: String::from("model"),
            max_context: Some(100_000),
            usage: kraai_types::TokenUsage {
                input_tokens: 20_000,
                ..Default::default()
            },
        }),
        ..AppState::default()
    };
    for (provider, model, expected) in [
        (Some("provider"), Some("model"), "20,000/100,000 (20%)"),
        (Some("provider"), Some("other"), "20,000"),
        (Some("other"), Some("model"), "20,000"),
        (Some("provider"), None, "20,000"),
        (None, Some("model"), "20,000"),
        (None, None, "20,000"),
    ] {
        state.selected_provider_id = provider.map(String::from);
        state.selected_model_id = model.map(String::from);
        assert_eq!(statusline_context_label(&state), expected);
    }
}

#[test]
fn footer_truncation_preserves_graphemes_and_terminal_width() {
    assert_eq!(fit("你好 world", 4), "你…");
    assert_eq!(fit("e\u{301} long", 2), "e\u{301}…");
    assert_eq!(fit("anything", 0), "");
    assert_eq!(fit("hello\nworld", 20), "hello world");
}
