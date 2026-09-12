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
        29
    );
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn narrow_status_keeps_error_ahead_of_model_metadata() {
    let state = AppState {
        last_error: Some(String::from("Stream error: offline")),
        ..AppState::default()
    };
    assert!(screen(&state, 28, 12).contains("Stream error: offline"));
    for (phase, streaming, retry, expected) in [
        (ScriptPhase::Idle, true, false, "generating"),
        (ScriptPhase::Executing, false, false, "executing script"),
        (ScriptPhase::Idle, true, true, "retrying"),
        (
            ScriptPhase::AwaitingApproval,
            false,
            false,
            "approval required",
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
        matches!(harness.requests_rx.try_recv(), Ok(RuntimeRequest::LoadSession { session_id }) if session_id == "beta")
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
    assert!(chat.height >= 15);
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
            ),
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
    assert!(collapsed.contains("125 ms total"));
    assert!(!collapsed.contains("secret-source"));
    assert!(!collapsed.contains("secret-output"));
    harness.app.select_execution(true);
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
        *output = output.replace("exit_code=\"0\"", "exit_code=\"1\"");
    }
    harness.app.invalidate_chat_cache();
    assert!(screen(&harness.app.state, 100, 30).contains("secret-output"));
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
                ),
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
