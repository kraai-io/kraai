use super::*;
use crate::app::{KeyCode, KeyEvent, KeyModifiers, UiMode};
use kraai_types::{ScriptCall, SessionId};
use ratatui::layout::Rect;

#[test]
fn approval_scroll_reaches_end_and_moves_back_with_metadata_pinned() {
    let mut harness = test_harness();
    let mut script = pending_script("long");
    script.source = (0..100)
        .map(|line| format!("echo line-{line:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    harness.app.state.pending_script = Some(script);
    harness.app.enter_script_decision_phase();
    assert!(screen(&harness.app.state, 90, 30).contains("line-000"));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    let rendered = screen(&harness.app.state, 90, 30);
    assert!(rendered.contains("line-099"));
    assert!(rendered.contains("Additional: workspace-write"));
    let end = harness.app.state.approval_scroll.get();
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(harness.app.state.approval_scroll.get(), end - 1);
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
    assert!(harness.app.state.approval_expanded);
    assert_eq!(
        crate::app::ui::bottom_panel_height(&harness.app.state, Rect::new(0, 0, 90, 30)),
        28
    );
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn narrow_status_keeps_error_ahead_of_model_metadata() {
    let state = AppState {
        last_error: Some(String::from("Stream error: offline")),
        ..AppState::default()
    };
    assert!(screen(&state, 28, 12).contains("/error"));
    for (phase, streaming, retry, expected) in [
        (ScriptPhase::Idle, true, false, "Working"),
        (ScriptPhase::Executing, false, false, "Running"),
        (ScriptPhase::Idle, true, true, "Retrying"),
        (
            ScriptPhase::AwaitingApproval,
            false,
            false,
            "Needs approval",
        ),
    ] {
        let state = AppState {
            script_phase: phase,
            is_streaming: streaming,
            retry_waiting: retry,
            ..AppState::default()
        };
        assert!(screen(&state, 60, 15).contains(expected));
    }
}

#[test]
fn composer_scroll_keeps_cursor_and_tail_visible() {
    let input = (0..50)
        .map(|line| format!("line-{line:03} 你好"))
        .collect::<Vec<_>>()
        .join("\n");
    let state = AppState {
        input_cursor: input.len(),
        input,
        ..AppState::default()
    };
    let area = Rect::new(0, 0, 40, 24);
    let [chat, _, composer] = crate::app::ui::chat_layout(&state, area);
    assert!(chat.height >= 14);
    assert!(screen(&state, 40, 24).contains("line-049"));
    let cursor = crate::components::TextInput::new(&state.input, state.input_cursor)
        .get_cursor_position(composer);
    assert!(composer.contains(cursor.into()));
    for height in 1..6 {
        for width in 1..6 {
            let _ = screen(&state, width, height);
        }
    }
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "rendering failures propagate while layout assertions fail the test"
)]
fn clearing_multiline_input_restores_three_row_background() -> color_eyre::Result<()> {
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    let mut harness = test_harness();
    for width in [8, 40, 80] {
        for height in [8, 24, 50] {
            let mut terminal = Terminal::new(TestBackend::new(width, height))?;
            harness
                .app
                .set_input_text("many\nlines\nof\ninput\nhere".to_string());
            terminal.draw(|frame| frame.render_widget(&harness.app.state, frame.area()))?;
            harness.app.clear_message_draft();
            terminal.draw(|frame| frame.render_widget(&harness.app.state, frame.area()))?;

            let area = Rect::new(0, 0, width, height);
            let [_, _, input] = crate::app::ui::chat_layout(&harness.app.state, area);
            assert_eq!(input.height, 3);
            assert_eq!(input.bottom(), height);
            assert_eq!(
                crate::components::TextInput::new(&harness.app.state.input, 0)
                    .get_cursor_position(input),
                (1, height - 2)
            );
            let buffer = terminal.backend().buffer();
            for y in 0..height {
                for x in 0..width {
                    assert_eq!(
                        buffer[(x, y)].bg == Color::DarkGray,
                        y >= height - 3,
                        "unexpected input background at ({x}, {y}) in {width}x{height}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn word_editing_preserves_unicode() {
    let mut harness = test_harness();
    harness.app.set_input_text(String::from("hello 你好 world"));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(harness.app.state.input_cursor, "hello 你好 ".len());
    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        harness
            .app
            .handle_key_event(KeyEvent::new(KeyCode::Char('w'), modifiers));
        assert_eq!(harness.app.state.input, "hello 你好 world");
    }
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
    assert_eq!(harness.app.state.input, "hello world");
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL));
    assert_eq!(harness.app.state.input, "hello ");
}

#[test]
fn ctrl_o_expands_script_source_and_output_in_the_chat_and_keeps_the_draft() {
    use kraai_types::{AssistantItem, ConversationItem, ToolCallId};
    let mut harness = test_harness();
    let call = Message {
        id: MessageId::new("call"),
        parent_id: None,
        content: ConversationItem::Assistant {
            items: vec![AssistantItem::ScriptCall(ScriptCall {
                call_id: ToolCallId::new("tool"),
                name: String::from("nushell"),
                input: String::from("echo secret-source"),
                arguments_error: None,
                rewrite: None,
            })],
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    };
    let result = Message {
        id: MessageId::new("result"),
        parent_id: Some(call.id.clone()),
        content: ConversationItem::ScriptResult {
            outcome: kraai_execution_types::ScriptExecutionOutcome {
                status: kraai_execution_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
            call_id: ToolCallId::new("tool"),
            output: String::from(
                "<tool_call_result status=\"completed\" exit_code=\"0\" elapsed_ms=\"125\">\n<stdout>secret-output</stdout>\n</tool_call_result>",
            ).into(),
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    };
    harness.app.state.chat_history.insert(call.id.clone(), call);
    harness
        .app
        .state
        .chat_history
        .insert(result.id.clone(), result);
    harness.app.state.input = String::from("draft-to-preserve");
    let collapsed = screen(&harness.app.state, 100, 30);
    assert!(collapsed.contains("1 script · 0.1s  [ctrl+o]"));
    assert!(!collapsed.contains("secret-source"));
    assert!(!collapsed.contains("secret-output"));
    let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    harness.app.handle_key_event(ctrl_o);
    let expanded = screen(&harness.app.state, 100, 30);
    assert!(expanded.contains("secret-source"));
    assert!(expanded.contains("secret-output"));
    assert!(!expanded.contains("[ctrl+o]"));
    assert!(expanded.contains("draft-to-preserve"));
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    harness.app.handle_key_event(ctrl_o);
    assert_eq!(screen(&harness.app.state, 100, 30), collapsed);
    assert_eq!(harness.app.state.input, "draft-to-preserve");
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn model_search_preserves_order_unicode_and_cross_field_matches() {
    let mut harness = test_harness();
    let model = |id: &str, name: &str| kraai_runtime::Model {
        options: Vec::new(),
        id: id.to_owned(),
        name: name.to_owned(),
        max_context: Some(1000),
    };
    harness.app.state.models_by_provider = HashMap::from([
        (String::from("zeta"), vec![model("z-model", "Last")]),
        (
            String::from("Alpha"),
            vec![model("Z-β", "ÉLAN 模型"), model("A-id", "Second")],
        ),
    ]);
    for (query, expected) in [
        (
            "",
            vec![("Alpha", "Z-β"), ("Alpha", "A-id"), ("zeta", "z-model")],
        ),
        (
            " ",
            vec![("Alpha", "Z-β"), ("Alpha", "A-id"), ("zeta", "z-model")],
        ),
        ("ALPHA Z-Β", vec![("Alpha", "Z-β")]),
        ("Β ÉLAN", vec![("Alpha", "Z-β")]),
        ("模型", vec![("Alpha", "Z-β")]),
        ("ZETA Z-MODEL", vec![("zeta", "z-model")]),
        ("alpha  z", Vec::new()),
        ("missing", Vec::new()),
    ] {
        harness.app.state.sheet_query = query.to_owned();
        let actual: Vec<_> = harness
            .app
            .state
            .filtered_models()
            .into_iter()
            .map(|(provider, model)| (provider, model.id.as_str()))
            .collect();
        assert_eq!(actual, expected, "query: {query}");
    }
    harness.app.state.mode = UiMode::Model;
    harness.app.state.sheet_query = String::from("Β ÉLAN");
    harness
        .app
        .handle_model_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        harness.app.state.selected_provider_id.as_deref(),
        Some("Alpha")
    );
    assert_eq!(harness.app.state.selected_model_id.as_deref(), Some("Z-β"));
    assert_eq!(harness.app.state.mode, UiMode::Chat);
}

#[test]
fn model_refresh_preserves_selection_and_clamps_missing_or_filtered_entries() {
    let mut harness = test_harness();
    let models = |ids: &[&str]| {
        HashMap::from([(
            String::from("provider"),
            ids.iter()
                .map(|id| kraai_runtime::Model {
                    options: Vec::new(),
                    id: (*id).to_owned(),
                    name: (*id).to_owned(),
                    max_context: None,
                })
                .collect(),
        )])
    };
    harness.app.state.selected_provider_id = Some(String::from("provider"));
    harness.app.state.selected_model_id = Some(String::from("keep"));
    harness.app.state.models_by_provider = models(&["keep", "target"]);
    harness.app.state.mode = UiMode::Model;
    harness.app.state.sheet_index = 1;

    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models(&[
            "other", "keep", "target",
        ]))));
    assert_eq!(harness.app.state.sheet_index, 2);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models(&["other", "keep"]))));
    assert_eq!(harness.app.state.sheet_index, 0);

    harness.app.state.sheet_index = 99;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models(&["keep", "other"]))));
    assert_eq!(harness.app.state.sheet_index, 1);

    harness.app.state.sheet_query = String::from("missing");
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models(&["keep", "other"]))));
    assert_eq!(harness.app.state.sheet_index, 0);
    harness.app.state.sheet_query.clear();
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(HashMap::new())));
    assert_eq!(harness.app.state.sheet_index, 0);
    assert_eq!(harness.app.state.selected_model_id.as_deref(), Some("keep"));
}

#[test]
fn model_search_matches_provider_name_and_id_with_empty_results() {
    let mut harness = test_harness();
    harness.app.state.mode = UiMode::Model;
    harness.app.state.models_by_provider.insert(
        String::from("provider"),
        vec![kraai_runtime::Model {
            options: Vec::new(),
            id: String::from("model-id"),
            name: String::from("Display name"),
            max_context: Some(1000),
        }],
    );
    for query in ["PROVIDER", "model-id", "display"] {
        harness.app.state.sheet_query = query.to_string();
        assert_eq!(harness.app.state.filtered_models().len(), 1);
    }
    harness.app.state.sheet_query = String::from("missing");
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(harness.app.state.selected_model_id.is_none());
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn error_details_preserve_draft_mode_and_pending_approval() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("draft");
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;
    harness.app.set_error(String::from(
        "first
second
final",
    ));
    harness.app.open_error();
    assert!(screen(&harness.app.state, 80, 20).contains("final"));
    harness.app.handle_paste(String::from("hidden edit"));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(harness.app.state.input, "draft");
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!harness.app.state.error_open);
    assert!(harness.app.state.last_error.is_some());
    assert_eq!(
        harness.app.state.script_phase,
        ScriptPhase::AwaitingApproval
    );
    assert!(harness.drain_requests().is_empty());
    harness.app.open_error();
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    assert!(harness.app.state.last_error.is_none());
}

#[test]
fn error_dialog_ctrl_c_closes_without_cancelling_or_dismissing() {
    let mut harness = test_harness();
    harness.app.set_error(String::from("error"));
    harness.app.state.error_open = true;
    harness.app.state.input = String::from("draft");
    for (key, modifiers) in [('d', KeyModifiers::CONTROL), ('c', KeyModifiers::ALT)] {
        harness
            .app
            .handle_key_event(KeyEvent::new(KeyCode::Char(key), modifiers));
        assert!(harness.app.state.error_open);
        assert_eq!(harness.app.state.last_error.as_deref(), Some("error"));
    }
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(!harness.app.state.error_open);
    assert_eq!(harness.app.state.last_error.as_deref(), Some("error"));
    assert_eq!(harness.app.state.input, "draft");
    assert!(!harness.app.state.exit);
    assert!(!harness.app.state.ctrl_c_exit_armed);
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn error_details_reach_end_of_large_error_and_resize() {
    let mut harness = test_harness();
    let error = format!(
        "{}last error line",
        "wide 你好 error
"
        .repeat(66_000)
    );
    harness.app.set_error(error);
    harness.app.open_error();
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert!(screen(&harness.app.state, 60, 12).contains("last error line"));
    assert!(harness.app.state.error_scroll.get() > 65_535);
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(harness.app.state.error_scroll.get(), 0);
    harness
        .app
        .set_error(String::from("small error for resize"));
    for width in 0..6 {
        for height in 0..6 {
            screen(&harness.app.state, width, height);
        }
    }
}

#[test]
fn shift_enter_inserts_newline_without_submitting_a_command() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("/undo");
    harness.app.state.input_cursor = 5;
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(harness.app.state.input, "/undo\n");
    assert_eq!(harness.app.state.input_cursor, 6);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn pasted_line_endings_are_preserved_as_newlines() {
    let mut harness = test_harness();
    harness
        .app
        .handle_paste(String::from("one\r\ntwo\rthree\nfour"));
    assert_eq!(harness.app.state.input, "one\ntwo\nthree\nfour");
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn animation_and_copy_feedback_expire_without_idle_redraws() {
    use crate::app::feedback::CopyTarget;
    let mut harness = test_harness();
    let now = std::time::Instant::now();
    harness.app.state.is_streaming = true;
    assert!(!harness.app.advance_statusline_animation(now));
    assert!(
        !harness
            .app
            .advance_statusline_animation(now + std::time::Duration::from_millis(119))
    );
    assert!(
        harness
            .app
            .advance_statusline_animation(now + std::time::Duration::from_millis(120))
    );
    assert_eq!(harness.app.state.statusline_animation_frame, 1);
    for frame in crate::app::ui::STATUSLINE_STREAMING_FRAMES {
        assert_eq!(crate::components::display_width(frame), 3);
    }
    harness.app.state.is_streaming = false;
    assert!(harness.app.advance_statusline_animation(now));
    harness.app.state.feedback.completion_until = Some(now + std::time::Duration::from_secs(1));
    harness
        .app
        .state
        .feedback
        .show_copied(CopyTarget::Error, now);
    harness.app.state.last_error = Some(String::from("An error"));
    harness.app.state.error_open = true;
    assert!(screen(&harness.app.state, 80, 20).contains("Copied"));
    assert!(!harness.app.state.feedback.copied(CopyTarget::Auth));
    assert!(
        harness
            .app
            .advance_statusline_animation(now + std::time::Duration::from_secs(1))
    );
    assert!(harness.app.state.feedback.completion_until.is_none());
    assert!(harness.app.state.feedback.copied(CopyTarget::Error));
    assert!(
        harness
            .app
            .advance_statusline_animation(now + std::time::Duration::from_millis(1200))
    );
    assert!(screen(&harness.app.state, 80, 20).contains("c copy"));
    assert!(
        !harness
            .app
            .advance_statusline_animation(now + std::time::Duration::from_secs(2))
    );
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "fixture parsing errors propagate while feedback assertions fail the test"
)]
fn only_newly_finished_turns_show_completion_feedback() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    let timer = |active: bool| {
        serde_json::from_value::<kraai_runtime::TurnTimer>(serde_json::json!({
            "started_at": null,
            "accumulated": std::time::Duration::from_secs(2),
            "active": active,
            "last_duration": if active { None } else { Some(std::time::Duration::from_secs(2)) },
        }))
    };
    harness.app.handle_runtime_event(Event::TurnTimingChanged {
        session_id: SessionId::new("session"),
        timer: timer(false)?,
    });
    assert!(harness.app.state.feedback.completion_until.is_none());
    harness.app.handle_runtime_event(Event::TurnTimingChanged {
        session_id: SessionId::new("session"),
        timer: timer(true)?,
    });
    harness.app.handle_runtime_event(Event::TurnTimingChanged {
        session_id: SessionId::new("session"),
        timer: timer(false)?,
    });
    assert!(harness.app.state.feedback.completion_until.is_some());
    let completed = screen(&harness.app.state, 100, 20);
    assert!(
        completed
            .lines()
            .any(|line| line.starts_with(" Finished in 2s"))
    );
    assert!(!completed.contains("✓"));
    harness.app.state.feedback.completion_until = None;
    let settled = screen(&harness.app.state, 100, 20);
    assert_eq!(
        completed
            .split_once("Finished in")
            .map(|(prefix, _)| crate::components::display_width(prefix)),
        settled
            .split_once("Finished in")
            .map(|(prefix, _)| crate::components::display_width(prefix)),
    );
    harness.app.state.feedback.completion_until = Some(std::time::Instant::now());
    harness.app.set_error(String::from("Failed"));
    assert!(harness.app.state.feedback.completion_until.is_none());
    harness.app.state.last_error = None;
    harness.app.state.feedback.completion_until = Some(std::time::Instant::now());
    harness.app.handle_runtime_event(Event::StreamCancelled {
        session_id: SessionId::new("session"),
        message_id: String::from("message"),
    });
    assert!(harness.app.state.feedback.completion_until.is_none());
    assert!(!screen(&harness.app.state, 100, 20).contains('✓'));
    Ok(())
}
