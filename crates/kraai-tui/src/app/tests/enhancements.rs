use super::*;
use crate::app::{KeyCode, KeyEvent, KeyModifiers, UiMode};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

fn screen(state: &AppState, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    state.render(area, &mut buffer);
    buffer
        .content()
        .chunks(width.max(1) as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

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
    assert!(screen(&state, 28, 12).contains("F8 error"));
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
fn session_filter_selects_and_deletes_the_visible_session() {
    let mut harness = test_harness();
    harness.app.state.mode = UiMode::SessionsMenu;
    let mut first = session_snapshot(None).session;
    first.id = String::from("alpha");
    first.title = Some(String::from("First"));
    let mut second = first.clone();
    second.id = String::from("beta");
    second.title = Some(String::from("Xylophone"));
    harness.app.state.sessions = vec![first, second];
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(harness.drain_requests().is_empty());
    assert_eq!(harness.app.state.filtered_sessions().len(), 1);
    assert_eq!(harness.app.state.sessions_menu_index, 1);
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(harness.requests_rx.try_recv(), Ok(RuntimeRequest::LoadSession { session_id, .. }) if session_id == "beta")
    );
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert!(
        matches!(harness.requests_rx.try_recv(), Ok(RuntimeRequest::DeleteSession { session_id }) if session_id == "beta")
    );
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
fn word_editing_preserves_unicode_and_editor_shortcut_does_not_insert_text() {
    let mut harness = test_harness();
    harness.app.set_input_text(String::from("hello 你好 world"));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(harness.app.state.input_cursor, "hello 你好 ".len());
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(harness.app.state.input, "hello world");
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL));
    assert_eq!(harness.app.state.input, "hello ");
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert!(harness.app.state.editor_requested);
    assert_eq!(harness.app.state.input, "hello ");
}

#[test]
fn executions_collapse_success_expand_failure_and_retain_source() {
    use kraai_types::{AssistantItem, ConversationItem, ToolCallId};
    let mut harness = test_harness();
    let call = Message {
        id: MessageId::new("call"),
        parent_id: None,
        content: ConversationItem::Assistant {
            items: vec![AssistantItem::ScriptCall {
                call_id: ToolCallId::new("tool"),
                name: String::from("nushell"),
                input: String::from("echo secret-source"),
            }],
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    };
    let result = Message {
        id: MessageId::new("result"),
        parent_id: Some(call.id.clone()),
        content: ConversationItem::ScriptResult {
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
    let collapsed = screen(&harness.app.state, 100, 30);
    assert!(collapsed.contains("0.1s"));
    assert!(!collapsed.contains("secret-source"));
    assert!(!collapsed.contains("secret-output"));
    harness.app.open_executions();
    harness.app.toggle_execution();
    let expanded = screen(&harness.app.state, 100, 30);
    assert!(expanded.contains("secret-source"));
    assert!(expanded.contains("secret-output"));
    harness.app.state.execution_expanded.clear();
    if let Some(message) = harness
        .app
        .state
        .chat_history
        .get_mut(&MessageId::new("result"))
        && let ConversationItem::ScriptResult { output, .. } = &mut message.content
    {
        *output = output
            .display_text()
            .replace("exit_code=\"0\"", "exit_code=\"1\"")
            .into();
    }
    harness.app.invalidate_chat_cache();
    assert!(!screen(&harness.app.state, 100, 30).contains("secret-output"));
}

#[test]
fn execution_toggle_recovers_selection_after_tip_changes() {
    use kraai_types::{ConversationItem, ToolCallId};
    let mut harness = test_harness();
    for (id, parent_id) in [("first", None), ("second", Some("first"))] {
        let message = Message {
            id: MessageId::new(id),
            parent_id: parent_id.map(MessageId::new),
            content: ConversationItem::ScriptResult {
                call_id: ToolCallId::new(id),
                output: String::from(
                    "<tool_call_result status=\"completed\" exit_code=\"0\"></tool_call_result>",
                )
                .into(),
            },
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        };
        harness
            .app
            .state
            .chat_history
            .insert(message.id.clone(), message);
    }
    harness.app.state.current_tip_id = Some(String::from("second"));
    harness.app.select_execution(false);
    assert_eq!(
        harness.app.state.selected_execution.as_deref(),
        Some("second")
    );
    harness.app.state.current_tip_id = Some(String::from("first"));
    harness
        .app
        .state
        .chat_history
        .remove(&MessageId::new("second"));
    harness.app.invalidate_chat_cache();
    harness.app.toggle_execution();
    assert_eq!(
        harness.app.state.selected_execution.as_deref(),
        Some("first")
    );
    assert_eq!(
        harness.app.state.execution_expanded.get("first"),
        Some(&true)
    );
    assert!(!harness.app.state.execution_expanded.contains_key("second"));

    harness.app.state.chat_history.clear();
    harness.app.invalidate_chat_cache();
    harness.app.toggle_execution();
    assert!(harness.app.state.selected_execution.is_none());
    assert_eq!(
        harness.app.state.execution_expanded.get("first"),
        Some(&true)
    );
}

#[test]
fn model_search_preserves_order_unicode_and_cross_field_matches() {
    let mut harness = test_harness();
    let model = |id: &str, name: &str| kraai_runtime::Model {
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
        harness.app.state.menu_search = query.to_owned();
        let actual: Vec<_> = harness
            .app
            .state
            .filtered_models()
            .into_iter()
            .map(|(provider, model)| (provider, model.id.as_str()))
            .collect();
        assert_eq!(actual, expected, "query: {query}");
    }

    harness.app.startup_options.ci = true;
    harness.app.state.mode = UiMode::ModelMenu;
    harness.app.state.menu_search = String::from("Β ÉLAN");
    harness
        .app
        .handle_model_menu_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        harness.app.state.selected_provider_id.as_deref(),
        Some("Alpha")
    );
    assert_eq!(harness.app.state.selected_model_id.as_deref(), Some("Z-β"));
    assert_eq!(
        harness.app.state.status,
        "Selected model: Alpha / ÉLAN 模型"
    );
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
    harness.app.state.model_menu_index = 1;

    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models(&[
            "other", "keep", "target",
        ]))));
    assert_eq!(harness.app.state.model_menu_index, 2);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models(&["other", "keep"]))));
    assert_eq!(harness.app.state.model_menu_index, 0);

    harness.app.state.model_menu_index = 99;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models(&["keep", "other"]))));
    assert_eq!(harness.app.state.model_menu_index, 1);

    harness.app.state.menu_search = String::from("missing");
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models(&["keep", "other"]))));
    assert_eq!(harness.app.state.model_menu_index, 0);
    harness.app.state.menu_search.clear();
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(HashMap::new())));
    assert_eq!(harness.app.state.model_menu_index, 0);
    assert_eq!(harness.app.state.selected_model_id.as_deref(), Some("keep"));
}

#[test]
fn model_search_matches_provider_name_and_id_with_empty_results() {
    let mut harness = test_harness();
    harness.app.state.mode = UiMode::ModelMenu;
    harness.app.state.models_by_provider.insert(
        String::from("provider"),
        vec![kraai_runtime::Model {
            id: String::from("model-id"),
            name: String::from("Display name"),
            max_context: Some(1000),
        }],
    );
    for query in ["PROVIDER", "model-id", "display"] {
        harness.app.state.menu_search = query.to_string();
        assert_eq!(harness.app.state.filtered_models().len(), 1);
    }
    harness.app.state.menu_search = String::from("missing");
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
fn execution_view_navigates_from_latest_and_preserves_draft() {
    use kraai_types::{ConversationItem, ToolCallId};
    let mut harness = test_harness();
    harness.app.state.input = String::from("draft-to-preserve");
    for (id, parent_id) in [("first", None), ("second", Some("first"))] {
        let message = Message {
            id: MessageId::new(id),
            parent_id: parent_id.map(MessageId::new),
            content: ConversationItem::ScriptResult {
                call_id: ToolCallId::new(id),
                output: String::from(
                    "<tool_call_result status=\"failed\">output-detail</tool_call_result>",
                )
                .into(),
            },
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        };
        harness
            .app
            .state
            .chat_history
            .insert(message.id.clone(), message);
    }
    harness.app.state.current_tip_id = Some(String::from("second"));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
    assert_eq!(harness.app.state.mode, UiMode::Executions);
    assert_eq!(
        harness.app.state.selected_execution.as_deref(),
        Some("second")
    );
    assert_eq!(
        crate::app::ui::bottom_panel_height(&harness.app.state, Rect::new(0, 0, 100, 30)),
        0
    );
    assert!(!screen(&harness.app.state, 100, 30).contains("draft-to-preserve"));
    harness.app.handle_paste(String::from("ignored"));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        harness.app.state.execution_expanded.get("second"),
        Some(&true)
    );
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(
        harness.app.state.selected_execution.as_deref(),
        Some("first")
    );
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        harness.app.state.execution_expanded.get("first"),
        Some(&true)
    );
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        harness.app.state.execution_expanded.get("first"),
        Some(&false)
    );
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(
        harness.app.state.selected_execution.as_deref(),
        Some("second")
    );
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.execution_expanded.is_empty());
    assert!(harness.app.state.selected_execution.is_none());
    assert_eq!(harness.app.state.input, "draft-to-preserve");
    assert!(!screen(&harness.app.state, 100, 30).contains("output-detail"));
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn approval_closes_execution_view_and_empty_view_can_exit() {
    let mut harness = test_harness();
    harness.app.open_executions();
    assert!(harness.app.state.selected_execution.is_none());
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    harness.app.open_executions();
    harness
        .app
        .state
        .execution_expanded
        .insert(String::from("old"), true);
    harness.app.enter_script_decision_phase();
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.execution_expanded.is_empty());
    assert_eq!(
        harness.app.state.script_phase,
        ScriptPhase::AwaitingApproval
    );
}

#[test]
fn delayed_profile_response_closes_execution_view() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.open_executions();
    harness
        .app
        .state
        .execution_expanded
        .insert(String::from("old"), true);
    harness.app.state.selected_execution = Some(String::from("old"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionProfile {
            session_id: String::from("session"),
            profile_id: String::from("agent"),
            result: Ok(()),
        });
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.execution_expanded.is_empty());
    assert!(harness.app.state.selected_execution.is_none());
}

#[test]
fn short_help_scrolls_to_final_binding_and_resets_on_reopen() {
    let mut harness = test_harness();
    harness.app.handle_command("help");
    let first = screen(&harness.app.state, 90, 8);
    assert!(first.contains("/ for commands"));
    assert!(!first.contains("Close / cancel"));
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert!(screen(&harness.app.state, 90, 8).contains("Close / cancel"));
    let end = harness.app.state.help_scroll.get();
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(harness.app.state.help_scroll.get(), end - 1);
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(screen(&harness.app.state, 90, 8).contains("Close / cancel"));
    screen(&harness.app.state, 90, 30);
    assert_eq!(harness.app.state.help_scroll.get(), 0);
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    screen(&harness.app.state, 90, 8);
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    harness.app.handle_command("help");
    assert_eq!(harness.app.state.help_scroll.get(), 0);
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
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
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
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
    harness.app.state.status = String::from("Another notice");
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    assert!(harness.app.state.last_error.is_none());
    assert_eq!(harness.app.state.status, "Another notice");
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
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
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
    harness.app.state.input = String::from("/help");
    harness.app.state.input_cursor = 5;
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_eq!(harness.app.state.input, "/help\n");
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
    harness.app.state.current_session_id = Some(String::from("session"));
    let timer = |active: bool| {
        serde_json::from_value::<kraai_runtime::TurnTimer>(serde_json::json!({
            "started_at": null,
            "accumulated": std::time::Duration::from_secs(2),
            "active": active,
            "last_duration": if active { None } else { Some(std::time::Duration::from_secs(2)) },
        }))
    };
    harness.app.handle_runtime_event(Event::TurnTimingChanged {
        session_id: String::from("session"),
        timer: timer(false)?,
    });
    assert!(harness.app.state.feedback.completion_until.is_none());
    harness.app.handle_runtime_event(Event::TurnTimingChanged {
        session_id: String::from("session"),
        timer: timer(true)?,
    });
    harness.app.handle_runtime_event(Event::TurnTimingChanged {
        session_id: String::from("session"),
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
        session_id: String::from("session"),
        message_id: String::from("message"),
    });
    assert!(harness.app.state.feedback.completion_until.is_none());
    assert!(!screen(&harness.app.state, 100, 20).contains('✓'));
    Ok(())
}
