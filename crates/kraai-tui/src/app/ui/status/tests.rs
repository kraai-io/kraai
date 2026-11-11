use super::*;
use ratatui::style::Color;

fn row(state: &AppState, width: u16) -> String {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
    render_status(state, buffer.area, &mut buffer);
    buffer.content().iter().map(|cell| cell.symbol()).collect()
}

fn colour_at(state: &AppState, width: u16, text: &str) -> Option<Color> {
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, 1));
    render_status(state, buffer.area, &mut buffer);
    let line: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
    let column = line.find(text).map(|at| line[..at].chars().count())?;
    Some(buffer[(u16::try_from(column).ok()?, 0)].fg)
}

fn context(state: &AppState) -> String {
    match context_facts(state) {
        (tokens, Some(percent)) => format!("{tokens} {percent}"),
        (tokens, None) => tokens,
    }
}

fn astra() -> AppState {
    AppState {
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
    }
}

#[test]
fn facts_start_one_column_in_and_the_turn_is_a_tag_against_the_right_edge() {
    let mut state = astra();
    assert_eq!(
        row(&state, 60).trim_end(),
        " Astra  136,000/272,000  50%  $0.0000"
    );
    assert_eq!(colour_at(&state, 60, "Astra"), Some(Color::Reset));
    assert_eq!(colour_at(&state, 60, "50%"), Some(theme::MUTED));

    state.is_streaming = true;
    assert_eq!(
        row(&state, 60),
        format!(
            " Astra  136,000/272,000  50%  $0.0000{}Working ·   ",
            " ".repeat(11)
        )
    );
    assert_eq!(colour_at(&state, 60, "Working"), Some(theme::ACCENT));
    state.retry_waiting = true;
    assert_eq!(colour_at(&state, 60, "Retrying"), Some(theme::WAITING));

    state.is_streaming = false;
    state.retry_waiting = false;
    state.script_phase = ScriptPhase::AwaitingApproval;
    assert!(row(&state, 60).ends_with("  Needs approval "));
    assert_eq!(colour_at(&state, 60, "Needs"), Some(theme::WAITING));
}

#[test]
fn a_missing_model_takes_the_waiting_colour() {
    let state = AppState::default();
    assert_eq!(
        row(&state, 60).trim_end(),
        " No model selected  0 tokens  $0.0000"
    );
    assert_eq!(colour_at(&state, 60, "No model"), Some(theme::WAITING));
}

#[test]
fn a_narrow_line_gives_up_facts_before_it_cuts_the_tag() {
    let mut state = astra();
    state.is_streaming = true;
    state.selected_profile_id = Some(String::from("Coding"));
    for (width, expected) in [
        (70, " Astra  Coding  136,000/272,000  50%  $0.0000"),
        (56, " Astra  Coding  50%  $0.0000"),
        (40, " Astra  50%  $0.0000"),
        (30, " Astra  50%  $0…"),
        (14, ""),
    ] {
        let line = row(&state, width);
        assert!(line.ends_with("Working ·   "), "{line}");
        assert_eq!(
            line.trim_end_matches("Working ·   ").trim_end(),
            expected,
            "{width}"
        );
    }
    assert_eq!(row(&state, 8), " Worki… ");
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
            assert!(rows.get(1).is_some_and(|row| row.contains("/error")));
        }
    }
}

#[test]
fn an_error_gets_a_red_row_above_the_facts_that_names_the_command_to_read_it() {
    let mut state = astra();
    state.last_error = Some(String::from("Stream error: offline\nsecond line"));
    let area = Rect::new(0, 0, 50, 2);
    let mut buffer = Buffer::empty(area);
    render_status(&state, area, &mut buffer);
    let rows: Vec<String> = buffer
        .content()
        .chunks(50)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect();
    assert_eq!(
        rows,
        [
            format!(" Stream error: offline{}/error ", " ".repeat(21)),
            format!("{:<50}", " Astra  136,000/272,000  50%  $0.0000"),
        ]
    );
    assert_eq!(buffer[(1, 0)].fg, theme::DANGER);
    assert_eq!(buffer[(43, 0)].fg, theme::DANGER);

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
        assert!(rows.first().is_some_and(|row| row.ends_with("…  /error ")));
        assert!(
            rows.get(1)
                .is_some_and(|row| row.contains("Astra") && !row.contains("/error"))
        );
    }
}

#[test]
fn the_line_draws_nothing_outside_a_tiny_area() {
    let mut state = astra();
    state.is_streaming = true;
    state.last_error = Some(String::from("error"));
    for (width, height) in [(0, 0), (1, 1), (2, 1), (3, 2), (1, 3)] {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        render_status(&state, buffer.area, &mut buffer);
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
        ("first", "model", "20,000/100,000 20%"),
        ("first", "other", "20,000/200,000 10%"),
        ("second", "model", "20,000/50,000 40%"),
        ("second", "unknown", "20,000 tokens"),
        ("first", "model", "20,000/100,000 20%"),
    ] {
        state.selected_provider_id = Some(provider.into());
        state.selected_model_id = Some(model.into());
        assert_eq!(context(&state), expected);
    }
    for (limit, expected) in [
        (Some(400_000), "20,000/400,000 5%"),
        (None, "20,000 tokens"),
    ] {
        for model in state.models_by_provider.values_mut().flatten() {
            model.max_context = limit;
        }
        assert_eq!(context(&state), expected);
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
        (Some("provider"), Some("model"), "20,000/100,000 20%"),
        (Some("provider"), Some("other"), "20,000 tokens"),
        (Some("other"), Some("model"), "20,000 tokens"),
        (Some("provider"), None, "20,000 tokens"),
        (None, Some("model"), "20,000 tokens"),
        (None, None, "20,000 tokens"),
    ] {
        state.selected_provider_id = provider.map(String::from);
        state.selected_model_id = model.map(String::from);
        assert_eq!(context(&state), expected);
    }
}

#[test]
fn footer_truncation_preserves_graphemes_and_terminal_width() {
    assert_eq!(fit("你好 world", 4), "你…");
    assert_eq!(fit("e\u{301} long", 2), "e\u{301}…");
    assert_eq!(fit("anything", 0), "");
    assert_eq!(fit("hello\nworld", 20), "hello world");
}
