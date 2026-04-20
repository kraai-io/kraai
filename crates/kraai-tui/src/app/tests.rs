use super::{
    ActiveSettingsEditor, App, AppState, ChatCellPosition, ChatSelection, OptimisticMessage,
    OptimisticToolMessage, PendingSubmit, PendingTool, ProviderAuthState, ProviderAuthStatus,
    ProvidersAdvancedFocus, ProvidersView, RuntimeRequest, RuntimeResponse,
    STATUSLINE_ANIMATION_INTERVAL, SettingsModelField, SettingsProviderField, StartupOptions,
    ToolPhase, UiMode, default_agent_profiles, is_copy_shortcut, menu_scroll_offset,
    model_menu_next_index, model_menu_previous_index, render_chat_selection_overlay,
    selection_text,
};
use crate::components::VisibleChatView;
use crossbeam_channel::{Receiver, unbounded};
use kraai_runtime::{
    AgentProfileSummary, AgentProfilesState, Event, FieldDefinition, FieldValueEntry,
    FieldValueKind, Model, ModelSettings, ProviderDefinition, ProviderSettings, Session,
    SettingsDocument, SettingsValue,
};
use kraai_types::{
    ChatRole, Message, MessageGeneration, MessageId, MessageStatus, ModelId, ProviderId, TokenUsage,
};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{
        Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;
struct TestHarness {
    app: App,
    requests_rx: Receiver<RuntimeRequest>,
}
#[derive(Clone, Default)]
struct SharedBuffer {
    data: Arc<Mutex<Vec<u8>>>,
}
impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.data
            .lock()
            .expect("buffer poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn test_harness() -> TestHarness {
    let (_event_tx, event_rx) = unbounded();
    let (runtime_tx, requests_rx) = unbounded();
    let (_response_tx, runtime_rx) = unbounded();
    TestHarness {
        app: App {
            event_rx,
            runtime_tx,
            runtime_rx,
            clipboard: None,
            ci_output: Box::new(io::sink()),
            ci_output_needs_newline: false,
            ci_turn_completion_pending: false,
            startup_options: StartupOptions::default(),
            startup_message_sent: false,
            ci_error: None,
            state: AppState::default(),
            last_stream_refresh: None,
            last_statusline_animation_tick: None,
        },
        requests_rx,
    }
}
fn test_harness_with_startup_options(startup_options: StartupOptions) -> TestHarness {
    let mut harness = test_harness();
    harness.app.startup_options = startup_options.clone();
    harness.app.state = AppState::from_startup_options(startup_options);
    harness
}
fn install_ci_output_capture(harness: &mut TestHarness) -> Arc<Mutex<Vec<u8>>> {
    let buffer = SharedBuffer::default();
    let data = buffer.data.clone();
    harness.app.ci_output = Box::new(buffer);
    data
}
fn captured_output(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8(buffer.lock().expect("buffer poisoned").clone()).expect("utf8 output")
}
impl TestHarness {
    fn drain_requests(&self) -> Vec<RuntimeRequest> {
        let mut requests = Vec::new();
        while let Ok(request) = self.requests_rx.try_recv() {
            requests.push(request);
        }
        requests
    }
    fn set_chat_metrics(&mut self, total_lines: u16, viewport_height: u16) {
        let mut cache = self.app.state.chat_render_cache.borrow_mut();
        cache.total_lines = total_lines;
        drop(cache);
        self.app.state.chat_viewport_height = viewport_height;
    }
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn shift_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}
fn ctrl_shift_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
}
fn ctrl_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}
fn message(id: &str, parent_id: Option<&str>, role: ChatRole, content: &str) -> Message {
    Message {
        id: MessageId::new(id),
        parent_id: parent_id.map(MessageId::new),
        role,
        content: content.to_string(),
        status: MessageStatus::Complete,
        agent_profile_id: None,
        tool_state_snapshot: None,
        tool_state_deltas: Vec::new(),
        generation: None,
    }
}

fn assistant_message_with_usage(
    id: &str,
    provider_id: &str,
    model_id: &str,
    usage: TokenUsage,
) -> Message {
    Message {
        id: MessageId::new(id),
        parent_id: None,
        role: ChatRole::Assistant,
        content: String::from("assistant"),
        status: MessageStatus::Complete,
        agent_profile_id: None,
        tool_state_snapshot: None,
        tool_state_deltas: Vec::new(),
        generation: Some(MessageGeneration {
            provider_id: ProviderId::new(provider_id),
            model_id: ModelId::new(model_id),
            max_context: Some(128_000),
            usage: Some(usage),
        }),
    }
}
fn sample_models() -> HashMap<String, Vec<Model>> {
    HashMap::from([(
        String::from("openai-chat-completions"),
        vec![
            Model {
                id: String::from("gpt-4.1-mini"),
                name: String::from("GPT-4.1 Mini"),
                max_context: Some(128_000),
            },
            Model {
                id: String::from("gpt-4o-mini"),
                name: String::from("GPT-4o Mini"),
                max_context: Some(128_000),
            },
        ],
    )])
}
fn sample_settings() -> SettingsDocument {
    SettingsDocument {
        providers: vec![ProviderSettings {
            id: String::from("openai-chat-completions"),
            type_id: String::from("openai-chat-completions"),
            values: vec![
                FieldValueEntry {
                    key: String::from("base_url"),
                    value: SettingsValue::String(String::from("https://api.openai.com/v1")),
                },
                FieldValueEntry {
                    key: String::from("env_var_api_key"),
                    value: SettingsValue::String(String::from("OPENAI_API_KEY")),
                },
                FieldValueEntry {
                    key: String::from("only_listed_models"),
                    value: SettingsValue::Bool(true),
                },
            ],
        }],
        models: vec![ModelSettings {
            id: String::from("gpt-4o-mini"),
            provider_id: String::from("openai-chat-completions"),
            values: vec![
                FieldValueEntry {
                    key: String::from("name"),
                    value: SettingsValue::String(String::from("GPT-4o Mini")),
                },
                FieldValueEntry {
                    key: String::from("max_context"),
                    value: SettingsValue::Integer(128_000),
                },
            ],
        }],
    }
}
fn sample_provider_definitions() -> Vec<ProviderDefinition> {
    vec![
        ProviderDefinition {
            type_id: String::from("openai-codex"),
            display_name: String::from("OpenAI Codex"),
            protocol_family: String::from("openai-responses"),
            description: String::from("ChatGPT subscription auth"),
            provider_fields: vec![],
            model_fields: vec![
                FieldDefinition {
                    key: String::from("name"),
                    label: String::from("Display Name"),
                    value_kind: FieldValueKind::String,
                    required: false,
                    secret: false,
                    help_text: None,
                    default_value: None,
                },
                FieldDefinition {
                    key: String::from("max_context"),
                    label: String::from("Max Context"),
                    value_kind: FieldValueKind::Integer,
                    required: false,
                    secret: false,
                    help_text: None,
                    default_value: None,
                },
            ],
            supports_model_discovery: true,
            default_provider_id_prefix: String::from("openai-codex"),
        },
        ProviderDefinition {
            type_id: String::from("openai-chat-completions"),
            display_name: String::from("OpenAI-compatible Chat Completions"),
            protocol_family: String::from("openai-chat-completions"),
            description: String::from("Test definition"),
            provider_fields: vec![
                FieldDefinition {
                    key: String::from("base_url"),
                    label: String::from("Base URL"),
                    value_kind: FieldValueKind::Url,
                    required: true,
                    secret: false,
                    help_text: None,
                    default_value: Some(SettingsValue::String(String::from(
                        "https://api.openai.com/v1",
                    ))),
                },
                FieldDefinition {
                    key: String::from("api_key"),
                    label: String::from("Inline API Key"),
                    value_kind: FieldValueKind::String,
                    required: false,
                    secret: true,
                    help_text: None,
                    default_value: None,
                },
                FieldDefinition {
                    key: String::from("env_var_api_key"),
                    label: String::from("Env Var"),
                    value_kind: FieldValueKind::String,
                    required: false,
                    secret: false,
                    help_text: None,
                    default_value: Some(SettingsValue::String(String::from("OPENAI_API_KEY"))),
                },
                FieldDefinition {
                    key: String::from("only_listed_models"),
                    label: String::from("Only Listed Models"),
                    value_kind: FieldValueKind::Boolean,
                    required: false,
                    secret: false,
                    help_text: None,
                    default_value: Some(SettingsValue::Bool(true)),
                },
            ],
            model_fields: vec![
                FieldDefinition {
                    key: String::from("name"),
                    label: String::from("Display Name"),
                    value_kind: FieldValueKind::String,
                    required: false,
                    secret: false,
                    help_text: None,
                    default_value: None,
                },
                FieldDefinition {
                    key: String::from("max_context"),
                    label: String::from("Max Context"),
                    value_kind: FieldValueKind::Integer,
                    required: false,
                    secret: false,
                    help_text: None,
                    default_value: None,
                },
            ],
            supports_model_discovery: true,
            default_provider_id_prefix: String::from("openai-chat-completions"),
        },
    ]
}
fn sample_sessions() -> Vec<Session> {
    vec![
        Session {
            id: String::from("sess-1"),
            tip_id: Some(String::from("m2")),
            workspace_dir: String::from("/tmp/project-a"),
            created_at: 1,
            updated_at: 2,
            title: Some(String::from("Refactor ideas")),
            selected_profile_id: Some(String::from("plan-code")),
            profile_locked: true,
            waiting_for_approval: true,
            is_streaming: true,
        },
        Session {
            id: String::from("sess-2"),
            tip_id: Some(String::from("m3")),
            workspace_dir: String::from("/tmp/project-b"),
            created_at: 3,
            updated_at: 4,
            title: Some(String::from("Testing plan")),
            selected_profile_id: Some(String::from("build-code")),
            profile_locked: false,
            waiting_for_approval: false,
            is_streaming: false,
        },
    ]
}
fn sample_agent_profiles() -> Vec<AgentProfileSummary> {
    default_agent_profiles()
}
fn sample_pending_tools() -> Vec<PendingTool> {
    vec![
        PendingTool {
            call_id: String::from("call-1"),
            tool_id: String::from("read_file"),
            args: String::from("{\"path\":\"src/app.rs\"}"),
            description: String::from("Inspect the app module"),
            risk_level: String::from("read_only_workspace"),
            reasons: vec![
                String::from("Reads local source files"),
                String::from("Needed to answer the user"),
            ],
            approved: Some(true),
            queue_order: 0,
        },
        PendingTool {
            call_id: String::from("call-2"),
            tool_id: String::from("write_file"),
            args: String::from("{\"path\":\"src/app.rs\",\"content\":\"...\"}"),
            description: String::from("Patch the app module"),
            risk_level: String::from("undoable_workspace_write"),
            reasons: vec![String::from("Updates tracked source code")],
            approved: None,
            queue_order: 1,
        },
    ]
}
fn populated_state() -> AppState {
    AppState {
        config_loaded: true,
        status: String::from("Ready"),
        models_by_provider: sample_models(),
        agent_profiles: sample_agent_profiles(),
        provider_definitions: sample_provider_definitions(),
        selected_profile_id: Some(String::from("plan-code")),
        selected_provider_id: Some(String::from("openai-chat-completions")),
        selected_model_id: Some(String::from("gpt-4o-mini")),
        pending_tools: sample_pending_tools(),
        sessions: sample_sessions(),
        current_session_id: Some(String::from("sess-2")),
        current_tip_id: Some(String::from("m2")),
        settings_draft: Some(sample_settings()),
        chat_history: BTreeMap::from([
            (
                MessageId::new("m1"),
                message("m1", None, ChatRole::User, "How should we test the TUI?"),
            ),
            (
                MessageId::new("m2"),
                message(
                    "m2",
                    Some("m1"),
                    ChatRole::Assistant,
                    "Use render tests, interaction tests, and a small number of end-to-end smoke tests.",
                ),
            ),
        ]),
        ..AppState::default()
    }
}
fn render_state_snapshot(state: &AppState, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    state.render(area, &mut buffer);
    buffer_to_snapshot(&buffer)
}
fn buffer_to_snapshot(buffer: &Buffer) -> String {
    let mut lines = Vec::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            lines.push(format!("{y:02}: {trimmed}"));
        }
    }
    lines.join("\n")
}
fn visible_chat_view(lines: &[&str], area: Rect) -> VisibleChatView {
    VisibleChatView::from_strings(area, lines)
}
fn assert_snapshot(actual: &str, expected: &str) {
    if actual != expected {
        panic!("snapshot mismatch\n--- actual ---\n{actual}\n--- expected ---\n{expected}");
    }
}
#[test]
fn model_menu_scroll_stays_at_top_when_selection_is_visible() {
    assert_eq!(menu_scroll_offset(3, 20, 8), 0);
}
#[test]
fn model_menu_scroll_follows_selection_past_bottom() {
    assert_eq!(menu_scroll_offset(9, 20, 8), 2);
}
#[test]
fn model_menu_scroll_clamps_to_max_scroll() {
    assert_eq!(menu_scroll_offset(19, 20, 8), 12);
}
#[test]
fn menu_scroll_with_zero_visible_lines_stays_at_top() {
    assert_eq!(menu_scroll_offset(10, 20, 0), 0);
}
#[test]
fn menu_scroll_when_content_fits_stays_at_top() {
    assert_eq!(menu_scroll_offset(4, 5, 8), 0);
}
#[test]
fn page_up_from_auto_scroll_uses_visible_bottom_instead_of_stale_scroll() {
    let mut harness = test_harness();
    harness.set_chat_metrics(20, 8);
    harness.app.state.auto_scroll = true;
    harness.app.state.scroll = 0;
    harness.app.handle_chat_key_event(key(KeyCode::PageUp));
    assert_eq!(harness.app.state.scroll, 2);
    assert!(!harness.app.state.auto_scroll);
}
#[test]
fn page_down_at_chat_bottom_reenables_auto_scroll() {
    let mut harness = test_harness();
    harness.set_chat_metrics(20, 8);
    harness.app.state.auto_scroll = false;
    harness.app.state.scroll = 11;
    harness.app.handle_chat_key_event(key(KeyCode::PageDown));
    assert_eq!(harness.app.state.scroll, 12);
    assert!(harness.app.state.auto_scroll);
}
#[test]
fn mouse_scroll_down_at_chat_bottom_reenables_auto_scroll() {
    let mut harness = test_harness();
    harness.set_chat_metrics(20, 8);
    harness.app.state.auto_scroll = false;
    harness.app.state.scroll = 12;
    harness.app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(harness.app.state.scroll, 12);
    assert!(harness.app.state.auto_scroll);
}
#[test]
fn page_up_saturates_at_zero() {
    let mut harness = test_harness();
    harness.set_chat_metrics(20, 8);
    harness.app.state.auto_scroll = false;
    harness.app.state.scroll = 4;
    harness.app.handle_chat_key_event(key(KeyCode::PageUp));
    assert_eq!(harness.app.state.scroll, 0);
    assert!(!harness.app.state.auto_scroll);
}
#[test]
fn clamp_chat_scroll_reduces_offset_when_viewport_grows() {
    let mut harness = test_harness();
    harness.set_chat_metrics(20, 8);
    harness.app.state.auto_scroll = false;
    harness.app.state.scroll = 12;
    harness.app.update_chat_viewport(12);
    assert_eq!(harness.app.state.scroll, 8);
}
#[test]
fn end_reenables_auto_scroll() {
    let mut harness = test_harness();
    harness.set_chat_metrics(20, 8);
    harness.app.state.auto_scroll = false;
    harness.app.state.scroll = 5;
    harness.app.handle_chat_key_event(key(KeyCode::End));
    assert!(harness.app.state.auto_scroll);
    assert_eq!(harness.app.state.scroll, 12);
}
#[test]
fn scrolling_back_to_bottom_restores_sticky_auto_scroll() {
    let mut harness = test_harness();
    harness.set_chat_metrics(20, 8);
    harness.app.state.auto_scroll = false;
    harness.app.state.scroll = 2;
    harness.app.handle_chat_key_event(key(KeyCode::PageDown));
    assert!(harness.app.state.auto_scroll);
    assert_eq!(harness.app.state.scroll, 12);
    harness.set_chat_metrics(24, 8);
    harness.app.clamp_chat_scroll();
    assert!(harness.app.state.auto_scroll);
    assert_eq!(harness.app.state.scroll, 16);
}
#[test]
fn model_menu_next_index_wraps_at_end() {
    assert_eq!(model_menu_next_index(4, 5), 0);
}
#[test]
fn model_menu_next_index_advances_within_bounds() {
    assert_eq!(model_menu_next_index(2, 5), 3);
}
#[test]
fn model_menu_previous_index_wraps_at_start() {
    assert_eq!(model_menu_previous_index(0, 5), 4);
}
#[test]
fn model_menu_previous_index_moves_back_within_bounds() {
    assert_eq!(model_menu_previous_index(3, 5), 2);
}
#[test]
fn startup_options_apply_initial_selections() {
    let state = AppState::from_startup_options(StartupOptions {
        ci: false,
        auto_approve: false,
        provider_id: Some(String::from("openai-chat-completions")),
        model_id: Some(String::from("gpt-4o-mini")),
        agent_profile_id: Some(String::from("build-code")),
        message: None,
    });
    assert_eq!(
        state.selected_provider_id.as_deref(),
        Some("openai-chat-completions")
    );
    assert_eq!(state.selected_model_id.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(state.selected_profile_id.as_deref(), Some("build-code"));
}
#[test]
fn ensure_selected_model_prefers_requested_provider() {
    let mut harness = test_harness();
    harness.app.state.models_by_provider = sample_models();
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.ensure_selected_model();
    assert_eq!(
        harness.app.state.selected_provider_id.as_deref(),
        Some("openai-chat-completions")
    );
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("gpt-4.1-mini")
    );
}
#[test]
fn sync_current_session_profile_preserves_preselected_profile_without_session() {
    let mut harness = test_harness();
    harness.app.state.selected_profile_id = Some(String::from("build-code"));
    harness.app.sync_current_session_profile_from_sessions();
    assert_eq!(
        harness.app.state.selected_profile_id.as_deref(),
        Some("build-code")
    );
    assert!(!harness.app.state.profile_locked);
}
#[test]
fn models_response_autosends_startup_message() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: false,
        auto_approve: false,
        provider_id: Some(String::from("openai-chat-completions")),
        model_id: Some(String::from("gpt-4o-mini")),
        agent_profile_id: Some(String::from("build-code")),
        message: Some(String::from("hello from startup")),
    });
    harness.app.state.config_loaded = true;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(sample_models())));
    assert_eq!(
        harness
            .app
            .state
            .pending_submit
            .as_ref()
            .map(|submit| submit.message.as_str()),
        Some("hello from startup")
    );
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::CreateSession]
    ));
}
#[test]
fn startup_message_treats_slash_prefix_as_literal_message() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: false,
        auto_approve: false,
        provider_id: Some(String::from("openai-chat-completions")),
        model_id: Some(String::from("gpt-4o-mini")),
        agent_profile_id: Some(String::from("build-code")),
        message: Some(String::from("/help")),
    });
    harness.app.state.config_loaded = true;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(sample_models())));
    assert_eq!(
        harness
            .app
            .state
            .pending_submit
            .as_ref()
            .map(|submit| submit.message.as_str()),
        Some("/help")
    );
    assert_eq!(harness.app.state.mode, UiMode::Chat);
}
#[test]
fn ci_rejects_unknown_provider_before_sending_startup_message() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        auto_approve: false,
        provider_id: Some(String::from("missing-provider")),
        model_id: Some(String::from("gpt-4o-mini")),
        agent_profile_id: Some(String::from("build-code")),
        message: Some(String::from("hello")),
    });
    harness.app.state.config_loaded = true;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(sample_models())));
    assert!(harness.app.state.exit);
    assert_eq!(
        harness.app.ci_error.as_deref(),
        Some("Unknown provider for --ci: missing-provider")
    );
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn ci_rejects_whitespace_only_startup_message() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        auto_approve: false,
        provider_id: Some(String::from("openai-chat-completions")),
        model_id: Some(String::from("gpt-4o-mini")),
        agent_profile_id: Some(String::from("build-code")),
        message: Some(String::from("   ")),
    });
    harness.app.state.config_loaded = true;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(sample_models())));
    assert!(harness.app.state.exit);
    assert_eq!(
        harness.app.ci_error.as_deref(),
        Some("Message cannot be empty")
    );
}
#[test]
fn ci_stream_complete_waits_for_turn_to_unlock() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        ..StartupOptions::default()
    });
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.is_streaming = true;
    harness.app.state.profile_locked = true;
    harness.app.handle_runtime_event(Event::StreamComplete {
        session_id: String::from("sess-2"),
        message_id: String::from("msg-1"),
    });
    assert!(!harness.app.state.exit);
    assert_eq!(harness.app.ci_error, None);
    assert!(harness.app.ci_turn_completion_pending);
}
#[test]
fn ci_finishes_after_synced_state_shows_turn_is_idle() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        ..StartupOptions::default()
    });
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.is_streaming = true;
    harness.app.state.profile_locked = true;
    harness.app.handle_runtime_event(Event::StreamComplete {
        session_id: String::from("sess-2"),
        message_id: String::from("msg-1"),
    });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::PendingTools {
            session_id: String::from("sess-2"),
            result: Ok(Vec::new()),
        });
    assert!(!harness.app.state.exit);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::AgentProfiles {
            session_id: String::from("sess-2"),
            result: Ok(AgentProfilesState {
                profiles: default_agent_profiles(),
                warnings: Vec::new(),
                selected_profile_id: Some(String::from("plan-code")),
                profile_locked: false,
            }),
        });
    assert!(harness.app.state.exit);
    assert_eq!(harness.app.ci_error, None);
    assert_eq!(harness.app.state.status, "CI run completed");
    assert!(!harness.app.ci_turn_completion_pending);
}
#[test]
fn ci_stream_chunk_prints_to_terminal_output() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        ..StartupOptions::default()
    });
    let output = install_ci_output_capture(&mut harness);
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_runtime_event(Event::StreamChunk {
        session_id: String::from("sess-2"),
        message_id: String::from("msg-1"),
        chunk: String::from("hello"),
    });
    assert_eq!(captured_output(&output), "hello");
}
#[test]
fn ci_stream_complete_finishes_terminal_line() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        ..StartupOptions::default()
    });
    let output = install_ci_output_capture(&mut harness);
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_runtime_event(Event::StreamChunk {
        session_id: String::from("sess-2"),
        message_id: String::from("msg-1"),
        chunk: String::from("hello"),
    });
    harness.app.handle_runtime_event(Event::StreamComplete {
        session_id: String::from("sess-2"),
        message_id: String::from("msg-1"),
    });
    assert_eq!(captured_output(&output), "hello\n");
}
#[test]
fn ci_failure_finishes_terminal_line_before_exiting() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        ..StartupOptions::default()
    });
    let output = install_ci_output_capture(&mut harness);
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_runtime_event(Event::StreamChunk {
        session_id: String::from("sess-2"),
        message_id: String::from("msg-1"),
        chunk: String::from("partial"),
    });
    harness.app.handle_runtime_event(Event::StreamError {
        session_id: String::from("sess-2"),
        message_id: String::from("msg-1"),
        error: String::from("boom"),
    });
    assert_eq!(captured_output(&output), "partial\n");
    assert_eq!(harness.app.ci_error.as_deref(), Some("Stream error: boom"));
}
#[test]
fn non_ci_and_stale_chunks_do_not_print_terminal_output() {
    let mut non_ci_harness = test_harness();
    let non_ci_output = install_ci_output_capture(&mut non_ci_harness);
    non_ci_harness.app.state.current_session_id = Some(String::from("sess-2"));
    non_ci_harness.app.handle_runtime_event(Event::StreamChunk {
        session_id: String::from("sess-2"),
        message_id: String::from("msg-1"),
        chunk: String::from("hello"),
    });
    assert_eq!(captured_output(&non_ci_output), "");
    let mut stale_harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        ..StartupOptions::default()
    });
    let stale_output = install_ci_output_capture(&mut stale_harness);
    stale_harness.app.state.current_session_id = Some(String::from("sess-2"));
    stale_harness.app.handle_runtime_event(Event::StreamChunk {
        session_id: String::from("sess-3"),
        message_id: String::from("msg-1"),
        chunk: String::from("hello"),
    });
    assert_eq!(captured_output(&stale_output), "");
}
#[test]
fn ci_tool_call_detection_fails_immediately() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        ci: true,
        ..StartupOptions::default()
    });
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_runtime_event(Event::ToolCallDetected {
        session_id: String::from("sess-2"),
        call_id: String::from("call-1"),
        tool_id: String::from("write_file"),
        args: String::from("{}"),
        description: String::from("Write a file"),
        risk_level: String::from("undoable_workspace_write"),
        reasons: vec![String::from("Need to edit source")],
        queue_order: 0,
    });
    assert!(harness.app.state.exit);
    assert_eq!(
        harness.app.ci_error.as_deref(),
        Some("CI mode does not support tool approval: write_file")
    );
}
#[test]
fn renders_chat_screen_snapshot() {
    let mut state = populated_state();
    state.input = String::from("Add tests for the settings menu");
    state.input_cursor = state.input.len();
    let rendered = render_state_snapshot(&state, 72, 18);
    assert_snapshot(
        &rendered,
        r#"01:  > How should we test the TUI?
04:  • Use render tests, interaction tests, and a small number of end-to-end
05:     smoke tests.
14: idle · openai-chat-completions/GPT-4o Mini · Plan Code · ctx 0/128,000 (
16:  > Add tests for the settings menu"#,
    );
}
#[test]
fn renders_cancelled_statusline_snapshot() {
    let mut state = populated_state();
    state.status = String::from("Stream cancelled");
    let rendered = render_state_snapshot(&state, 120, 18);
    assert!(rendered.contains(
            "cancelled · openai-chat-completions/GPT-4o Mini · Plan Code · ctx 0/128,000 (0%) · Stream cancelled"
        ));
}
#[test]
fn renders_retrying_statusline_snapshot() {
    let mut state = populated_state();
    state.retry_waiting = true;
    state.status = String::from("Provider error, retry #6 in 27s");
    let rendered = render_state_snapshot(&state, 120, 18);
    assert!(rendered.contains("⠋"));
    assert!(rendered.contains("Provider error, retry #6 in 27s"));
}
#[test]
fn renders_tool_execution_statusline_snapshot() {
    let mut state = populated_state();
    state.pending_tools = vec![sample_pending_tools()[0].clone()];
    state.tool_phase = ToolPhase::ExecutingBatch;
    state.tool_batch_execution_started = true;
    state.status = String::from("Executing 1 decided tool call(s)");
    let rendered = render_state_snapshot(&state, 140, 18);
    assert!(rendered.contains(
            "⠋ · openai-chat-completions/GPT-4o Mini · Plan Code · ctx 0/128,000 (0%) · Executing 1 decided tool call(s)"
        ));
}
#[test]
fn renders_statusline_with_context_usage_snapshot() {
    let mut state = populated_state();
    state.context_usage = Some(kraai_runtime::SessionContextUsage {
        provider_id: String::from("mock"),
        model_id: String::from("mock-model"),
        max_context: Some(128_000),
        usage: kraai_runtime::TokenUsage {
            total_tokens: 14_575,
            input_tokens: 10_000,
            output_tokens: 3_000,
            reasoning_tokens: 500,
            cache_read_tokens: 1_000,
            cache_write_tokens: 75,
        },
    });
    let rendered = render_state_snapshot(&state, 120, 18);
    assert!(rendered.contains(
        "idle · openai-chat-completions/GPT-4o Mini · Plan Code · ctx 14,575/128,000 (11%) · Ready"
    ));
}

#[test]
fn exit_token_usage_summary_accumulates_per_model_and_total_since_launch() {
    let mut harness = test_harness();
    let history_one = BTreeMap::from([
        (
            MessageId::new("msg-1"),
            assistant_message_with_usage(
                "msg-1",
                "openai-chat-completions",
                "gpt-4o-mini",
                TokenUsage {
                    total_tokens: 14_575,
                    input_tokens: 10_000,
                    output_tokens: 3_000,
                    reasoning_tokens: 500,
                    cache_read_tokens: 1_000,
                    cache_write_tokens: 75,
                },
            ),
        ),
        (
            MessageId::new("msg-2"),
            assistant_message_with_usage(
                "msg-2",
                "openai-chat-completions",
                "gpt-4.1-mini",
                TokenUsage {
                    total_tokens: 925,
                    input_tokens: 700,
                    output_tokens: 180,
                    reasoning_tokens: 20,
                    cache_read_tokens: 25,
                    cache_write_tokens: 0,
                },
            ),
        ),
    ]);
    let history_two = BTreeMap::from([(
        MessageId::new("msg-3"),
        assistant_message_with_usage(
            "msg-3",
            "openai-chat-completions",
            "gpt-4o-mini",
            TokenUsage {
                total_tokens: 425,
                input_tokens: 250,
                output_tokens: 120,
                reasoning_tokens: 40,
                cache_read_tokens: 10,
                cache_write_tokens: 5,
            },
        ),
    )]);

    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("background-session"),
            result: Ok(history_one.clone()),
        });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("background-session"),
            result: Ok(history_one),
        });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("foreground-session"),
            result: Ok(history_two),
        });

    assert_eq!(
        harness.app.exit_token_usage_summary().as_deref(),
        Some(
            "Token usage since launch:\n  openai-chat-completions/gpt-4.1-mini: total 925, input 700, output 180, reasoning 20, cached 25 (read 25, write 0)\n  openai-chat-completions/gpt-4o-mini: total 15,000, input 10,250, output 3,120, reasoning 540, cached 1,090 (read 1,010, write 80)\n  total: total 15,925, input 10,950, output 3,300, reasoning 560, cached 1,115 (read 1,035, write 80)"
        )
    );
}
#[test]
fn statusline_animation_advances_while_streaming() {
    let mut harness = test_harness();
    let start = Instant::now();
    harness.app.state.is_streaming = true;
    assert!(!harness.app.advance_statusline_animation(start));
    assert_eq!(harness.app.state.statusline_animation_frame, 0);
    assert!(
        harness
            .app
            .advance_statusline_animation(start + STATUSLINE_ANIMATION_INTERVAL)
    );
    assert_eq!(harness.app.state.statusline_animation_frame, 1);
}
#[test]
fn statusline_animation_advances_while_tools_execute() {
    let mut harness = test_harness();
    let start = Instant::now();
    harness.app.state.tool_phase = ToolPhase::ExecutingBatch;
    assert!(!harness.app.advance_statusline_animation(start));
    assert_eq!(harness.app.state.statusline_animation_frame, 0);
    assert!(
        harness
            .app
            .advance_statusline_animation(start + STATUSLINE_ANIMATION_INTERVAL)
    );
    assert_eq!(harness.app.state.statusline_animation_frame, 1);
}
#[test]
fn statusline_animation_resets_when_runtime_becomes_idle() {
    let mut harness = test_harness();
    harness.app.state.tool_phase = ToolPhase::ExecutingBatch;
    harness.app.state.statusline_animation_frame = 3;
    harness.app.last_statusline_animation_tick = Some(Instant::now());
    harness.app.state.tool_phase = ToolPhase::Idle;
    assert!(harness.app.advance_statusline_animation(Instant::now()));
    assert_eq!(harness.app.state.statusline_animation_frame, 0);
    assert!(harness.app.last_statusline_animation_tick.is_none());
}
#[test]
fn tool_execution_phase_counts_as_runtime_active_without_start_flag() {
    let mut harness = test_harness();
    harness.app.state.tool_phase = ToolPhase::ExecutingBatch;
    harness.app.state.tool_batch_execution_started = false;
    assert!(harness.app.state.runtime_is_active());
}
#[test]
fn locked_profile_waiting_for_continuation_counts_as_runtime_active() {
    let mut harness = test_harness();
    harness.app.state.profile_locked = true;
    harness.app.state.tool_phase = ToolPhase::Idle;
    harness.app.state.is_streaming = false;
    harness.app.state.retry_waiting = false;
    assert!(harness.app.state.runtime_is_active());
}
#[test]
fn waiting_for_tool_approval_does_not_count_as_runtime_active() {
    let mut harness = test_harness();
    harness.app.state.profile_locked = true;
    harness.app.state.tool_phase = ToolPhase::Deciding;
    assert!(!harness.app.state.runtime_is_active());
}
#[test]
fn retry_waiting_clears_when_stream_starts() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-current"));
    harness.app.state.retry_waiting = true;
    harness.app.handle_runtime_event(Event::StreamStart {
        session_id: String::from("sess-current"),
        message_id: String::from("m-retry"),
    });
    assert!(!harness.app.state.retry_waiting);
    assert!(harness.app.state.is_streaming);
}
#[test]
fn provider_retry_event_updates_status_text() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-current"));
    harness
        .app
        .handle_runtime_event(Event::ProviderRetryScheduled {
            session_id: String::from("sess-current"),
            provider_id: String::from("openai"),
            model_id: String::from("gpt-4.1"),
            operation: String::from("responses"),
            retry_number: 6,
            delay_seconds: 27,
            reason: String::from("HTTP 429"),
        });
    assert!(harness.app.state.retry_waiting);
    assert_eq!(harness.app.state.status, "Provider error, retry #6 in 27s");
}
#[test]
fn reset_chat_session_clears_retry_waiting() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-current"));
    harness.app.state.retry_waiting = true;
    harness.app.state.is_streaming = true;
    harness
        .app
        .reset_chat_session(Some(String::from("sess-next")), "Session loaded");
    assert_eq!(
        harness.app.state.current_session_id.as_deref(),
        Some("sess-next")
    );
    assert!(!harness.app.state.retry_waiting);
    assert!(!harness.app.state.is_streaming);
    assert_eq!(harness.app.state.status, "Session loaded");
}
#[test]
fn renders_command_popup_snapshot() {
    let mut state = populated_state();
    state.input = String::from("/s");
    state.input_cursor = state.input.len();
    let rendered = render_state_snapshot(&state, 72, 18);
    assert_snapshot(
        &rendered,
        r#"01:  > How should we test the TUI?
04:  • Use render tests, interaction tests, and a small number of end-to-end
05:     smoke tests.
12:  ┌Command (Tab/Down next, Shift-Tab/Up prev┐
13:  │> /sessions  Open sessions menu          │
14: i└─────────────────────────────────────────┘ Plan Code · ctx 0/128,000 (
16:  > /s"#,
    );
}
#[test]
fn renders_model_menu_snapshot() {
    let mut state = populated_state();
    state.mode = UiMode::ModelMenu;
    state.model_menu_index = 1;
    let rendered = render_state_snapshot(&state, 72, 18);
    assert_snapshot(
        &rendered,
        r#"01:  > How should we test the TUI?
04:  • Use re┌/model──────────────────────────────────────────────┐nd-to-end
05:     smoke│Select model (Enter to choose, Esc to close)        │
06:          │  openai-chat-completions / GPT-4.1 Mini            │
07:          │> openai-chat-completions / GPT-4o Mini (current)   │
08:          │                                                    │
09:          │                                                    │
10:          │                                                    │
11:          │                                                    │
12:          └────────────────────────────────────────────────────┘
14: idle · openai-chat-completions/GPT-4o Mini · Plan Code · ctx 0/128,000 (
16:  >"#,
    );
}
#[test]
fn renders_providers_list_snapshot() {
    let mut state = populated_state();
    state.mode = UiMode::ProvidersMenu;
    state.providers_view = ProvidersView::List;
    let rendered = render_state_snapshot(&state, 100, 22);
    assert_snapshot(
        &rendered,
        r#"01:  > How should we test the TUI?
02:     ┌/providers───────────────────────────────────────────────────────────────────────────────┐
03:     │Providers                                                                                │
04:  • U│Enter=open, a=connect, d=delete, r=refresh, Esc=close                                    │
05:     │┌───────────────────────────────────────────────────────────────────────────────────────┐│
06:     ││Configured providers                                                                   ││
07:     ││> id=openai-chat-completions  OpenAI-compatible Chat Completions                       ││
08:     ││type=openai-chat-completions  models=2                                                 ││
09:     ││                                                                                       ││
10:     ││                                                                                       ││
11:     ││                                                                                       ││
12:     ││                                                                                       ││
13:     ││                                                                                       ││
14:     │└───────────────────────────────────────────────────────────────────────────────────────┘│
15:     │┌───────────────────────────────────────────────────────────────────────────────────────┐│
16:     ││One provider panel at a time                                                           ││
17:     │└───────────────────────────────────────────────────────────────────────────────────────┘│
18: idle└─────────────────────────────────────────────────────────────────────────────────────────┘
20:  >"#,
    );
}
#[test]
fn provider_detail_keeps_openai_actions_and_id_visible() {
    let mut state = populated_state();
    state.mode = UiMode::ProvidersMenu;
    state.providers_view = ProvidersView::Detail;
    state.settings_provider_index = 1;
    state.settings_draft = Some(SettingsDocument {
        providers: vec![
            ProviderSettings {
                id: String::from("openai-chat-completions"),
                type_id: String::from("openai-chat-completions"),
                values: vec![],
            },
            ProviderSettings {
                id: String::from("openai"),
                type_id: String::from("openai-codex"),
                values: vec![],
            },
        ],
        models: vec![],
    });
    let rendered = render_state_snapshot(&state, 100, 18);
    assert!(rendered.contains("Provider: id=openai"));
    assert!(rendered.contains("State: Signed out"));
    assert!(rendered.contains("Actions"));
    assert!(rendered.contains("b browser sign-in"));
}
#[test]
fn provider_detail_browser_pending_hides_raw_auth_url() {
    let mut state = populated_state();
    state.mode = UiMode::ProvidersMenu;
    state.providers_view = ProvidersView::Detail;
    state.settings_provider_index = 1;
    state.settings_draft = Some(SettingsDocument {
        providers: vec![
            ProviderSettings {
                id: String::from("openai-chat-completions"),
                type_id: String::from("openai-chat-completions"),
                values: vec![],
            },
            ProviderSettings {
                id: String::from("openai"),
                type_id: String::from("openai-codex"),
                values: vec![],
            },
        ],
        models: vec![],
    });
    state.openai_codex_auth = ProviderAuthStatus {
        state: ProviderAuthState::BrowserPending,
        auth_url: Some(String::from(
            "https://auth.openai.com/oauth/authorize?example=1",
        )),
        ..ProviderAuthStatus::default()
    };
    let rendered = render_state_snapshot(&state, 120, 22);
    assert!(rendered.contains("Browser should open automatically."));
    assert!(rendered.contains("y copy sign-in URL  o open again  x cancel"));
    assert!(!rendered.contains("https://auth.openai.com/oauth/authorize"));
}
#[test]
fn renders_connect_provider_snapshot() {
    let mut state = populated_state();
    state.mode = UiMode::ProvidersMenu;
    state.providers_view = ProvidersView::Connect;
    let rendered = render_state_snapshot(&state, 100, 22);
    assert!(rendered.contains("/providers"));
    assert!(rendered.contains("Connect a provider"));
    assert!(rendered.contains("OpenAI"));
}
#[test]
fn renders_sessions_menu_snapshot() {
    let mut state = populated_state();
    state.mode = UiMode::SessionsMenu;
    state.sessions_menu_index = 2;
    let rendered = render_state_snapshot(&state, 72, 18);
    assert_snapshot(
        &rendered,
        r#"01:  > How should we test the TUI?
04:  • Use ┌/sessions──────────────────────────────────────────────┐d-to-end
05:     smo│Sessions (Enter=load/new, x=delete, Esc=close)         │
06:        │  Start new chat                                       │
07:        │  Refactor ideas [approval] [streaming]                │
08:        │> Testing plan (current)                               │
09:        │                                                       │
10:        │                                                       │
11:        │                                                       │
12:        └───────────────────────────────────────────────────────┘
14: idle · openai-chat-completions/GPT-4o Mini · Plan Code · ctx 0/128,000 (
16:  >"#,
    );
}
#[test]
fn renders_tool_approval_panel_snapshot() {
    let mut state = populated_state();
    state.tool_phase = ToolPhase::Deciding;
    let rendered = render_state_snapshot(&state, 80, 20);
    assert_snapshot(
        &rendered,
        r#"01:  > How should we test the TUI?
04:  • Use render tests, interaction tests, and a small number of end-to-end smoke t
05:    ests.
09: idle · openai-chat-completions/GPT-4o Mini · Plan Code · ctx 0/128,000 (0%) · Re
10:   Permission required ─────────────────────────────────────────────────────────┐
11:   Patch the app module                                                         │
12:   tool: write_file  risk: undoable_workspace_write                             │
13:   why: Updates tracked source code                                             │
14:                                                                                │
15:   args                                                                         │
16:   {"path":"src/app.rs","content":"..."}                                        │
17:                                                                                │
18:    Allow   Reject                                           select <->  confir │
19:  ──────────────────────────────────────────────────────────────────────────────┘"#,
    );
}
#[test]
fn renders_help_menu_snapshot() {
    let mut state = populated_state();
    state.mode = UiMode::Help;
    let rendered = render_state_snapshot(&state, 72, 18);
    assert_snapshot(
        &rendered,
        r#"01:  > How should we test the TUI?
04:  • Use render ┌/help────────────────────────────────────┐r of end-to-end
05:     smoke test│Slash Commands                           │
06:               │/agent     Open agent selector           │
07:               │/continue  Reprompt the agent            │
08:               │/help      Open this help menu           │
09:               │/model     Open model selector           │
10:               │/new       Start a new chat              │
11:               │/providers Open providers                │
12:               └─────────────────────────────────────────┘
14: idle · openai-chat-completions/GPT-4o Mini · Plan Code · ctx 0/128,000 (
16:  >"#,
    );
}
#[test]
fn selection_text_preserves_visible_line_breaks() {
    let view = visible_chat_view(&["alpha", "", "beta"], Rect::new(0, 0, 10, 3));
    let selection = ChatSelection {
        anchor: ChatCellPosition { line: 0, column: 2 },
        focus: ChatCellPosition { line: 2, column: 2 },
    };
    assert_eq!(
        selection_text(&view, selection),
        Some(String::from("pha\n\nbet"))
    );
}
#[test]
fn copy_shortcut_requires_control_and_shift() {
    assert!(is_copy_shortcut(ctrl_shift_key(KeyCode::Char('c'))));
    assert!(!is_copy_shortcut(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL
    )));
}
#[test]
fn copy_selection_uses_rendered_chat_text() {
    let mut harness = test_harness();
    harness.app.state.visible_chat_view = Some(visible_chat_view(
        &["alpha beta", "gamma"],
        Rect::new(0, 0, 12, 2),
    ));
    harness.app.state.selection = Some(ChatSelection {
        anchor: ChatCellPosition { line: 0, column: 6 },
        focus: ChatCellPosition { line: 1, column: 2 },
    });
    let mut copied = None;
    let result = harness.app.copy_selection_with(|text| {
        copied = Some(text.to_string());
        Ok(())
    });
    assert_eq!(result, Ok(true));
    assert_eq!(copied, Some(String::from("beta\ngam")));
}
#[test]
fn first_ctrl_c_clears_chat_input_without_exiting() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("/s");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.state.command_popup_dismissed = true;
    harness.app.state.command_completion_prefix = Some(String::from("s"));
    harness.app.state.command_completion_index = 1;
    harness.app.state.selection = Some(ChatSelection {
        anchor: ChatCellPosition { line: 0, column: 0 },
        focus: ChatCellPosition { line: 0, column: 1 },
    });
    harness.app.state.visible_chat_view =
        Some(visible_chat_view(&["alpha"], Rect::new(0, 0, 10, 1)));
    harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
    assert!(!harness.app.state.exit);
    assert!(harness.app.state.ctrl_c_exit_armed);
    assert!(harness.app.state.input.is_empty());
    assert_eq!(harness.app.state.input_cursor, 0);
    assert_eq!(harness.app.state.selection, None);
    assert_eq!(harness.app.state.visible_chat_view, None);
    assert!(!harness.app.state.command_popup_dismissed);
    assert_eq!(harness.app.state.command_completion_prefix, None);
    assert_eq!(harness.app.state.command_completion_index, 0);
    assert_eq!(
        harness.app.state.status,
        "Cleared input. Press Ctrl+C again to exit"
    );
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn second_consecutive_ctrl_c_exits_chat() {
    let mut harness = test_harness();
    harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
    harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
    assert!(harness.app.state.exit);
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn non_ctrl_c_key_disarms_ctrl_c_exit_confirmation() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("hello");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
    assert!(harness.app.state.ctrl_c_exit_armed);
    harness.app.handle_key_event(key(KeyCode::Char('x')));
    assert!(!harness.app.state.ctrl_c_exit_armed);
    assert_eq!(harness.app.state.input, "x");
    harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
    assert!(!harness.app.state.exit);
    assert!(harness.app.state.ctrl_c_exit_armed);
    assert!(harness.app.state.input.is_empty());
}
#[test]
fn ctrl_c_in_non_chat_mode_preserves_existing_behavior() {
    let mut harness = test_harness();
    harness.app.state.mode = UiMode::Help;
    harness.app.handle_key_event(ctrl_key(KeyCode::Char('c')));
    assert_eq!(harness.app.state.mode, UiMode::Help);
    assert!(!harness.app.state.exit);
    assert!(!harness.app.state.ctrl_c_exit_armed);
}
#[test]
fn mouse_drag_updates_selection_in_chat_view() {
    let mut harness = test_harness();
    harness.app.state.visible_chat_view = Some(visible_chat_view(
        &["alpha", "beta"],
        Rect::new(0, 0, 10, 2),
    ));
    harness.app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    harness.app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 2,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        harness.app.state.selection,
        Some(ChatSelection {
            anchor: ChatCellPosition { line: 0, column: 1 },
            focus: ChatCellPosition { line: 1, column: 2 },
        })
    );
}
#[test]
fn selection_overlay_marks_buffer_cells() {
    let area = Rect::new(0, 0, 8, 2);
    let view = visible_chat_view(&["alpha", "beta"], area);
    let mut buffer = Buffer::empty(area);
    for y in 0..area.height {
        for x in 0..area.width {
            buffer[(x, y)]
                .set_char(' ')
                .set_style(Style::default().fg(Color::White));
        }
    }
    render_chat_selection_overlay(
        Some(&view),
        Some(ChatSelection {
            anchor: ChatCellPosition { line: 0, column: 1 },
            focus: ChatCellPosition { line: 1, column: 1 },
        }),
        &mut buffer,
    );
    assert_eq!(buffer[(1, 0)].bg, Color::Cyan);
    assert_eq!(buffer[(1, 0)].fg, Color::Black);
    assert_eq!(buffer[(1, 1)].bg, Color::Cyan);
}
#[test]
fn tab_cycles_slash_command_and_enter_executes_selected_command() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("/s");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Tab));
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert_eq!(harness.app.state.mode, UiMode::SessionsMenu);
    assert!(harness.app.state.input.is_empty());
    let requests = harness.drain_requests();
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0], RuntimeRequest::ListSessions));
}
#[test]
fn new_command_clears_local_state_without_creating_session() {
    let mut harness = test_harness();
    harness.app.state = populated_state();
    harness.app.state.retry_waiting = true;
    harness.app.state.pending_submit = Some(PendingSubmit {
        session_id: None,
        message: String::from("stale"),
        model_id: String::from("gpt-4o-mini"),
        provider_id: String::from("openai-chat-completions"),
    });
    harness.app.handle_command("new");
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(harness.app.state.current_session_id, None);
    assert_eq!(harness.app.state.current_tip_id, None);
    assert!(harness.app.state.chat_history.is_empty());
    assert!(harness.app.state.optimistic_messages.is_empty());
    assert!(harness.app.state.pending_submit.is_none());
    assert!(!harness.app.state.retry_waiting);
    assert_eq!(
        harness.app.state.selected_profile_id.as_deref(),
        Some("plan-code")
    );
    assert_eq!(harness.app.state.status, "Started new chat");
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn agent_command_opens_selector_without_session() {
    let mut harness = test_harness();
    harness.app.handle_command("agent");
    assert_eq!(harness.app.state.mode, UiMode::AgentMenu);
    assert_eq!(harness.app.state.agent_profiles.len(), 2);
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn submit_without_session_requests_session_creation() {
    let mut harness = test_harness();
    harness.app.state.config_loaded = true;
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.input = String::from("hello world");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert_eq!(harness.app.state.status, "Creating session");
    assert_eq!(
        harness
            .app
            .state
            .pending_submit
            .as_ref()
            .map(|submit| submit.message.as_str()),
        Some("hello world")
    );
    let requests = harness.drain_requests();
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0], RuntimeRequest::CreateSession));
}
#[test]
fn create_session_applies_draft_agent_before_sending() {
    let mut harness = test_harness();
    harness.app.state.config_loaded = true;
    harness.app.state.selected_profile_id = Some(String::from("build-code"));
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.input = String::from("hello world");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::CreateSession]
    ));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::CreateSession(Ok(String::from("sess-3"))));
    let requests = harness.drain_requests();
    assert!(requests.iter().any(|request| {
        matches!(
            request,
            RuntimeRequest::SetSessionProfile { session_id, profile_id }
                if session_id == "sess-3" && profile_id == "build-code"
        )
    }));
    assert!(
        !requests
            .iter()
            .any(|request| { matches!(request, RuntimeRequest::SendMessage { .. }) })
    );
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionProfile {
            profile_id: String::from("build-code"),
            result: Ok(()),
        });
    let requests = harness.drain_requests();
    assert!(requests.iter().any(|request| {
        matches!(
            request,
            RuntimeRequest::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
                auto_approve,
            } if session_id == "sess-3"
                && message == "hello world"
                && model_id == "gpt-4o-mini"
                && provider_id == "openai-chat-completions"
                && !*auto_approve
        )
    }));
}
#[test]
fn sessions_menu_new_chat_starts_lazy_draft() {
    let mut harness = test_harness();
    harness.app.state = populated_state();
    harness.app.state.mode = UiMode::SessionsMenu;
    harness.app.state.sessions_menu_index = 0;
    harness
        .app
        .handle_sessions_menu_key_event(key(KeyCode::Enter));
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(harness.app.state.status, "Started new chat");
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn submit_sends_message_request_and_tracks_optimistic_message() {
    let mut harness = test_harness();
    harness.app.state.config_loaded = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.selected_profile_id = Some(String::from("plan-code"));
    harness.app.state.input = String::from("hello world");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert!(harness.app.state.input.is_empty());
    assert!(harness.app.state.is_streaming);
    assert_eq!(harness.app.state.optimistic_messages.len(), 1);
    assert_eq!(
        harness.app.state.optimistic_messages[0].content,
        "hello world"
    );
    let requests = harness.drain_requests();
    assert_eq!(requests.len(), 1);
    match &requests[0] {
        RuntimeRequest::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
            auto_approve,
        } => {
            assert_eq!(session_id, "sess-2");
            assert_eq!(message, "hello world");
            assert_eq!(model_id, "gpt-4o-mini");
            assert_eq!(provider_id, "openai-chat-completions");
            assert!(!auto_approve);
        }
        other => panic!("unexpected request: {}", request_name(other)),
    }
}
#[test]
fn submit_propagates_auto_approve_startup_option() {
    let mut harness = test_harness_with_startup_options(StartupOptions {
        auto_approve: true,
        ..StartupOptions::default()
    });
    harness.app.state.config_loaded = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.selected_profile_id = Some(String::from("plan-code"));
    harness.app.state.input = String::from("hello world");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Enter));
    let requests = harness.drain_requests();
    assert!(matches!(
        requests.as_slice(),
        [RuntimeRequest::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
            auto_approve,
        }] if session_id == "sess-2"
            && message == "hello world"
            && model_id == "gpt-4o-mini"
            && provider_id == "openai-chat-completions"
            && *auto_approve
    ));
}
#[test]
fn submit_unknown_slash_command_sends_message() {
    let mut harness = test_harness();
    harness.app.state.config_loaded = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.selected_profile_id = Some(String::from("plan-code"));
    harness.app.state.input = String::from("/not-a-command");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert!(harness.app.state.input.is_empty());
    assert!(harness.app.state.is_streaming);
    assert_eq!(harness.app.state.optimistic_messages.len(), 1);
    assert_eq!(
        harness.app.state.optimistic_messages[0].content,
        "/not-a-command"
    );
    let requests = harness.drain_requests();
    assert_eq!(requests.len(), 1);
    match &requests[0] {
        RuntimeRequest::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
            auto_approve,
        } => {
            assert_eq!(session_id, "sess-2");
            assert_eq!(message, "/not-a-command");
            assert_eq!(model_id, "gpt-4o-mini");
            assert_eq!(provider_id, "openai-chat-completions");
            assert!(!auto_approve);
        }
        other => panic!("unexpected request: {}", request_name(other)),
    }
}
#[test]
fn shift_enter_in_chat_inserts_newline_without_submitting() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("hello");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(shift_key(KeyCode::Enter));
    assert_eq!(harness.app.state.input, "hello\n");
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn paste_with_newlines_in_chat_inserts_text_without_submitting() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("before ");
    harness.app.state.input_cursor = harness.app.state.input.len();
    let changed = harness
        .app
        .handle_terminal_event(CrosstermEvent::Paste(String::from("line 1\nline 2")));
    assert!(changed);
    assert_eq!(harness.app.state.input, "before line 1\nline 2");
    assert_eq!(
        harness.app.state.input_cursor,
        harness.app.state.input.len()
    );
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn paste_inserts_text_at_cursor_position() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("hello world");
    harness.app.state.input_cursor = "hello ".len();
    let changed = harness
        .app
        .handle_terminal_event(CrosstermEvent::Paste(String::from("big\n")));
    assert!(changed);
    assert_eq!(harness.app.state.input, "hello big\nworld");
    assert_eq!(harness.app.state.input_cursor, "hello big\n".len());
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn escape_closes_providers_editor_before_leaving_advanced_view() {
    let mut harness = test_harness();
    harness.app.state.mode = UiMode::ProvidersMenu;
    harness.app.state.providers_view = ProvidersView::Advanced;
    harness.app.state.providers_advanced_focus = ProvidersAdvancedFocus::ProviderFields;
    harness.app.state.settings_draft = Some(sample_settings());
    harness.app.state.settings_editor =
        Some(ActiveSettingsEditor::Provider(SettingsProviderField::Id));
    harness.app.state.settings_editor_input = String::from("draft-openai");
    harness.app.handle_key_event(key(KeyCode::Esc));
    assert_eq!(harness.app.state.mode, UiMode::ProvidersMenu);
    assert_eq!(harness.app.state.settings_editor, None);
    assert!(harness.app.state.settings_editor_input.is_empty());
    harness.app.handle_key_event(key(KeyCode::Esc));
    assert_eq!(harness.app.state.providers_view, ProvidersView::Detail);
    assert_eq!(harness.app.state.status, "Back to provider detail");
}
#[test]
fn escape_dismisses_command_popup_in_chat() {
    let mut harness = test_harness();
    harness.app.state.input = String::from("/s");
    harness.app.state.input_cursor = harness.app.state.input.len();
    assert!(harness.app.command_popup_visible());
    harness.app.handle_key_event(key(KeyCode::Esc));
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.command_popup_dismissed);
    assert!(!harness.app.command_popup_visible());
    assert_eq!(harness.app.state.input, "/s");
}
#[test]
fn escape_dismisses_command_popup_before_cancelling_stream() {
    let mut harness = test_harness();
    harness.app.state.is_streaming = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.input = String::from("/s");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Esc));
    assert!(harness.app.state.command_popup_dismissed);
    assert!(harness.app.state.is_streaming);
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn escape_cancels_stream_when_chat_input_is_active() {
    let mut harness = test_harness();
    harness.app.state.is_streaming = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_key_event(key(KeyCode::Esc));
    let requests = harness.drain_requests();
    assert!(matches!(
        requests.as_slice(),
        [RuntimeRequest::CancelStream { session_id }] if session_id == "sess-2"
    ));
}
#[test]
fn escape_cancels_retry_wait_when_chat_input_is_active() {
    let mut harness = test_harness();
    harness.app.state.retry_waiting = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_key_event(key(KeyCode::Esc));
    let requests = harness.drain_requests();
    assert!(matches!(
        requests.as_slice(),
        [RuntimeRequest::CancelStream { session_id }] if session_id == "sess-2"
    ));
}
#[test]
fn escape_then_typing_partial_command_submits_message() {
    let mut harness = test_harness();
    harness.app.state.config_loaded = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.selected_profile_id = Some(String::from("plan-code"));
    harness.app.state.input = String::from("/s");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Esc));
    harness.app.handle_key_event(key(KeyCode::Char('e')));
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert!(harness.app.state.input.is_empty());
    assert!(harness.app.state.is_streaming);
    assert_eq!(harness.app.state.optimistic_messages.len(), 1);
    assert_eq!(harness.app.state.optimistic_messages[0].content, "/se");
    let requests = harness.drain_requests();
    assert_eq!(requests.len(), 1);
    match &requests[0] {
        RuntimeRequest::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
            auto_approve,
        } => {
            assert_eq!(session_id, "sess-2");
            assert_eq!(message, "/se");
            assert_eq!(model_id, "gpt-4o-mini");
            assert_eq!(provider_id, "openai-chat-completions");
            assert!(!auto_approve);
        }
        other => panic!("unexpected request: {}", request_name(other)),
    }
}
#[test]
fn submit_while_streaming_queues_message_and_requests_send() {
    let mut harness = test_harness();
    harness.app.state.config_loaded = true;
    harness.app.state.is_streaming = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.selected_profile_id = Some(String::from("plan-code"));
    harness.app.state.input = String::from("keep this draft");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert!(harness.app.state.input.is_empty());
    assert_eq!(harness.app.state.status, "Queued message (1 queued)");
    assert_eq!(harness.app.state.optimistic_messages.len(), 1);
    assert_eq!(
        harness.app.state.optimistic_messages[0].content,
        "keep this draft"
    );
    assert!(harness.app.state.optimistic_messages[0].is_queued);
    let requests = harness.drain_requests();
    assert!(matches!(
        requests.as_slice(),
        [RuntimeRequest::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
            auto_approve,
        }] if session_id == "sess-2"
            && message == "keep this draft"
            && model_id == "gpt-4o-mini"
            && provider_id == "openai-chat-completions"
            && !*auto_approve
    ));
}
#[test]
fn submit_during_tool_execution_queues_message() {
    let mut harness = test_harness();
    harness.app.state.config_loaded = true;
    harness.app.state.tool_phase = ToolPhase::ExecutingBatch;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.selected_profile_id = Some(String::from("plan-code"));
    harness.app.state.input = String::from("queue this");
    harness.app.state.input_cursor = harness.app.state.input.len();
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert!(harness.app.state.input.is_empty());
    assert_eq!(harness.app.state.status, "Queued message (1 queued)");
    assert_eq!(harness.app.state.optimistic_messages.len(), 1);
    assert_eq!(
        harness.app.state.optimistic_messages[0].content,
        "queue this"
    );
    assert!(harness.app.state.optimistic_messages[0].is_queued);
    let requests = harness.drain_requests();
    assert!(matches!(
        requests.as_slice(),
        [RuntimeRequest::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
            auto_approve,
        }] if session_id == "sess-2"
            && message == "queue this"
            && model_id == "gpt-4o-mini"
            && provider_id == "openai-chat-completions"
            && !*auto_approve
    ));
}
#[test]
fn submit_while_streaming_allows_unbounded_queued_messages() {
    let mut harness = test_harness();
    harness.app.state.config_loaded = true;
    harness.app.state.is_streaming = true;
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.current_tip_id = Some(String::from("a1"));
    harness.app.state.chat_history = BTreeMap::from([(
        MessageId::new("a1"),
        Message {
            id: MessageId::new("a1"),
            parent_id: None,
            role: ChatRole::Assistant,
            content: String::from("streaming reply"),
            status: MessageStatus::Streaming {
                call_id: kraai_types::CallId::new("call-1"),
            },
            agent_profile_id: Some(String::from("plan-code")),
            tool_state_snapshot: None,
            tool_state_deltas: Vec::new(),
            generation: None,
        },
    )]);
    harness.app.state.selected_provider_id = Some(String::from("openai-chat-completions"));
    harness.app.state.selected_model_id = Some(String::from("gpt-4o-mini"));
    harness.app.state.selected_profile_id = Some(String::from("plan-code"));
    for idx in 0..3 {
        let text = format!("queued message {idx}");
        harness.app.state.input = text.clone();
        harness.app.state.input_cursor = text.len();
        harness.app.handle_key_event(key(KeyCode::Enter));
    }
    assert_eq!(harness.app.state.optimistic_messages.len(), 3);
    assert!(
        harness
            .app
            .state
            .optimistic_messages
            .iter()
            .all(|message| message.is_queued)
    );
    let rendered = harness.app.state.rendered_messages();
    assert_eq!(rendered[0].role, ChatRole::Assistant);
    assert!(matches!(
        rendered[0].status,
        MessageStatus::Streaming { .. }
    ));
    assert_eq!(rendered[1].role, ChatRole::User);
    assert!(rendered[1].content.contains("[queued]"));
    assert!(
        rendered
            .iter()
            .filter(|message| message.content.contains("[queued]"))
            .count()
            >= 3
    );
    assert!(harness.app.state.is_streaming);
    let requests = harness.drain_requests();
    let send_count = requests
        .iter()
        .filter(|request| matches!(request, RuntimeRequest::SendMessage { .. }))
        .count();
    assert_eq!(send_count, 3);
}
#[test]
fn settings_command_redirects_to_providers() {
    let mut harness = test_harness();
    harness.app.handle_command("settings");
    assert_eq!(
        harness.app.state.status,
        "Unknown command: /settings. Use /providers"
    );
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn undo_command_requests_runtime_undo_for_current_session() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_command("undo");
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::UndoLastUserMessage { session_id }] if session_id == "sess-2"
    ));
}
#[test]
fn undo_command_requires_existing_session() {
    let mut harness = test_harness();
    harness.app.handle_command("undo");
    assert_eq!(harness.app.state.status, "No session to undo");
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn undo_command_is_blocked_while_turn_is_active() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.is_streaming = true;
    harness.app.handle_command("undo");
    assert_eq!(
        harness.app.state.status,
        "Cannot undo while the current turn is active"
    );
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn continue_command_requests_runtime_continuation_for_current_session() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_command("continue");
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::ContinueSession { session_id }] if session_id == "sess-2"
    ));
}
#[test]
fn continue_command_requires_existing_session() {
    let mut harness = test_harness();
    harness.app.handle_command("continue");
    assert_eq!(harness.app.state.status, "No session to continue");
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn continue_command_is_blocked_while_turn_is_active() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.is_streaming = true;
    harness.app.handle_command("continue");
    assert_eq!(
        harness.app.state.status,
        "Cannot continue while the current turn is active"
    );
    assert!(harness.drain_requests().is_empty());
}
#[test]
fn final_tool_decision_starts_batch_execution() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.tool_phase = ToolPhase::Deciding;
    harness.app.state.pending_tools = vec![PendingTool {
        call_id: String::from("call-2"),
        tool_id: String::from("write_file"),
        args: String::from("{\"path\":\"src/app.rs\",\"content\":\"...\"}"),
        description: String::from("Patch the app module"),
        risk_level: String::from("undoable_workspace_write"),
        reasons: vec![String::from("Updates tracked source code")],
        approved: None,
        queue_order: 0,
    }];
    harness.app.handle_key_event(key(KeyCode::Enter));
    assert_eq!(harness.app.state.tool_phase, ToolPhase::Deciding);
    let requests = harness.drain_requests();
    assert_eq!(requests.len(), 1);
    assert!(matches!(
        &requests[0],
        RuntimeRequest::ApproveTool { session_id, call_id }
            if session_id == "sess-2" && call_id == "call-2"
    ));
}
#[test]
fn settings_save_error_populates_field_errors() {
    let mut harness = test_harness();
    harness.app.state.mode = UiMode::ProvidersMenu;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SaveSettings(Err(String::from(
            "providers[0].id: duplicate provider\nmodels[0].max_context: invalid context",
        ))));
    assert_eq!(
        harness.app.state.settings_errors.get("providers[0].id"),
        Some(&String::from("duplicate provider"))
    );
    assert_eq!(
        harness
            .app
            .state
            .settings_errors
            .get("models[0].max_context"),
        Some(&String::from("invalid context"))
    );
    assert!(
        harness
            .app
            .state
            .status
            .starts_with("Failed saving settings:")
    );
}
#[test]
fn chat_history_response_reconciles_matching_optimistic_messages() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-1"));
    harness
        .app
        .state
        .optimistic_messages
        .push(OptimisticMessage {
            local_id: String::from("local-user-1"),
            content: String::from("hello world"),
            content_key: String::from("hello world"),
            occurrence: 1,
            is_queued: false,
        });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("sess-1"),
            result: Ok(BTreeMap::from([(
                MessageId::new("m1"),
                message("m1", None, ChatRole::User, "hello world"),
            )])),
        });
    assert!(harness.app.state.optimistic_messages.is_empty());
    assert_eq!(harness.app.state.chat_history.len(), 1);
}
#[test]
fn reconcile_optimistic_messages_keeps_duplicate_content_until_occurrence_matches() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-1"));
    harness.app.state.current_tip_id = Some(String::from("m1"));
    harness.app.state.chat_history = BTreeMap::from([(
        MessageId::new("m1"),
        message("m1", None, ChatRole::User, "hello"),
    )]);
    harness
        .app
        .state
        .optimistic_messages
        .push(OptimisticMessage {
            local_id: String::from("local-user-1"),
            content: String::from("hello"),
            content_key: String::from("hello"),
            occurrence: 2,
            is_queued: true,
        });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("sess-1"),
            result: Ok(BTreeMap::from([(
                MessageId::new("m1"),
                message("m1", None, ChatRole::User, "hello"),
            )])),
        });
    assert_eq!(harness.app.state.optimistic_messages.len(), 1);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("sess-1"),
            result: Ok(BTreeMap::from([
                (
                    MessageId::new("m1"),
                    message("m1", None, ChatRole::User, "hello"),
                ),
                (
                    MessageId::new("m2"),
                    message("m2", Some("m1"), ChatRole::User, "hello"),
                ),
            ])),
        });
    assert!(harness.app.state.optimistic_messages.is_empty());
}
#[test]
fn queued_status_updates_after_reconciliation() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-1"));
    harness.app.state.status = String::from("Queued message (1 queued)");
    harness
        .app
        .state
        .optimistic_messages
        .push(OptimisticMessage {
            local_id: String::from("local-user-1"),
            content: String::from("hello world"),
            content_key: String::from("hello world"),
            occurrence: 1,
            is_queued: true,
        });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("sess-1"),
            result: Ok(BTreeMap::from([(
                MessageId::new("m1"),
                message("m1", None, ChatRole::User, "hello world"),
            )])),
        });
    assert!(harness.app.state.optimistic_messages.is_empty());
    assert_eq!(harness.app.state.status, "Queued messages sent");
}
#[test]
fn stale_chat_history_response_is_ignored() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.chat_history = BTreeMap::from([(
        MessageId::new("m-current"),
        message("m-current", None, ChatRole::Assistant, "current"),
    )]);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("sess-1"),
            result: Ok(BTreeMap::from([(
                MessageId::new("m-stale"),
                message("m-stale", None, ChatRole::Assistant, "stale"),
            )])),
        });
    assert!(
        harness
            .app
            .state
            .chat_history
            .contains_key(&MessageId::new("m-current"))
    );
    assert!(
        !harness
            .app
            .state
            .chat_history
            .contains_key(&MessageId::new("m-stale"))
    );
}
#[test]
fn stale_current_tip_response_is_ignored() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.current_tip_id = Some(String::from("tip-current"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::CurrentTip {
            session_id: String::from("sess-1"),
            result: Ok(Some(String::from("tip-stale"))),
        });
    assert_eq!(
        harness.app.state.current_tip_id.as_deref(),
        Some("tip-current")
    );
}
#[test]
fn undo_response_restores_input_and_requests_session_refresh() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::UndoLastUserMessage {
            session_id: String::from("sess-2"),
            result: Ok(Some(String::from("redo this"))),
        });
    assert_eq!(harness.app.state.input, "redo this");
    assert_eq!(harness.app.state.input_cursor, "redo this".len());
    assert_eq!(harness.app.state.status, "Restored last user message");
    let requests = harness.drain_requests();
    assert!(requests.iter().any(
            |request| matches!(request, RuntimeRequest::GetCurrentTip { session_id } if session_id == "sess-2")
        ));
    assert!(requests.iter().any(
            |request| matches!(request, RuntimeRequest::GetChatHistory { session_id } if session_id == "sess-2")
        ));
}
#[test]
fn pending_tools_response_for_background_session_is_ignored() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.pending_tools = sample_pending_tools();
    harness
        .app
        .handle_runtime_response(RuntimeResponse::PendingTools {
            session_id: String::from("sess-1"),
            result: Ok(vec![kraai_runtime::PendingToolInfo {
                call_id: String::from("call-other"),
                tool_id: String::from("read_file"),
                args: String::from("{}"),
                description: String::from("other"),
                risk_level: String::from("read_only_workspace"),
                reasons: Vec::new(),
                approved: None,
                queue_order: 0,
            }]),
        });
    assert_eq!(harness.app.state.pending_tools.len(), 2);
    assert_eq!(harness.app.state.pending_tools[0].call_id, "call-1");
}
#[test]
fn settings_response_opens_editor_with_clean_state() {
    let mut harness = test_harness();
    harness.app.state.settings_errors =
        HashMap::from([(String::from("providers[0].id"), String::from("old error"))]);
    harness.app.state.settings_editor = Some(ActiveSettingsEditor::Model(
        SettingsModelField::Value(String::from("name")),
    ));
    harness.app.state.settings_editor_input = String::from("stale");
    harness.app.state.settings_delete_armed = true;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Settings(Ok(sample_settings())));
    assert_eq!(harness.app.state.mode, UiMode::ProvidersMenu);
    assert_eq!(harness.app.state.providers_view, ProvidersView::List);
    assert_eq!(harness.app.state.status, "Providers loaded");
    assert!(harness.app.state.settings_errors.is_empty());
    assert_eq!(harness.app.state.settings_editor, None);
    assert!(harness.app.state.settings_editor_input.is_empty());
    assert!(!harness.app.state.settings_delete_armed);
}
#[test]
fn providers_command_requests_settings_definitions_and_auth_status() {
    let mut harness = test_harness();
    harness.app.handle_command("providers");
    assert_eq!(harness.app.state.mode, UiMode::ProvidersMenu);
    assert_eq!(harness.app.state.providers_view, ProvidersView::List);
    let requests = harness.drain_requests();
    assert_eq!(requests.len(), 3);
    assert!(matches!(
        requests[0],
        RuntimeRequest::ListProviderDefinitions
    ));
    assert!(matches!(requests[1], RuntimeRequest::GetSettings));
    assert!(matches!(
        requests[2],
        RuntimeRequest::GetOpenAiCodexAuthStatus
    ));
}
#[test]
fn connect_provider_autosaves_new_provider() {
    let mut harness = test_harness();
    harness.app.state.settings_draft = Some(sample_settings());
    harness.app.state.provider_definitions = sample_provider_definitions();
    harness.app.state.mode = UiMode::ProvidersMenu;
    harness.app.state.providers_view = ProvidersView::Connect;
    harness.app.handle_key_event(key(KeyCode::Enter));
    let requests = harness.drain_requests();
    assert!(requests.iter().any(|request| matches!(
        request,
        RuntimeRequest::SaveSettings { settings }
            if settings.providers.iter().any(|provider| provider.type_id == "openai-codex")
    )));
}
#[test]
fn provider_field_commit_autosaves_settings() {
    let mut harness = test_harness();
    harness.app.state.settings_draft = Some(sample_settings());
    harness.app.state.provider_definitions = sample_provider_definitions();
    harness.app.state.mode = UiMode::ProvidersMenu;
    harness.app.state.providers_view = ProvidersView::Advanced;
    harness.app.state.settings_editor =
        Some(ActiveSettingsEditor::Provider(SettingsProviderField::Id));
    harness.app.state.settings_editor_input = String::from("renamed-provider");
    harness.app.handle_key_event(key(KeyCode::Enter));
    let requests = harness.drain_requests();
    assert!(requests.iter().any(|request| matches!(
        request,
        RuntimeRequest::SaveSettings { settings }
            if settings.providers.iter().any(|provider| provider.id == "renamed-provider")
    )));
}
#[test]
fn auth_updated_event_refreshes_openai_status() {
    let mut harness = test_harness();
    harness
        .app
        .handle_runtime_event(Event::OpenAiCodexAuthUpdated {
            status: kraai_runtime::OpenAiCodexAuthStatus {
                state: kraai_runtime::OpenAiCodexLoginState::Authenticated,
                email: Some(String::from("dev@example.com")),
                plan_type: Some(String::from("Pro")),
                account_id: Some(String::from("acct_123")),
                last_refresh_unix: Some(42),
                error: None,
            },
        });
    assert_eq!(
        harness.app.state.openai_codex_auth.state,
        ProviderAuthState::Authenticated
    );
    assert_eq!(
        harness.app.state.openai_codex_auth.plan_type.as_deref(),
        Some("Pro")
    );
}
#[test]
fn approve_tool_response_marks_pending_tool_as_approved() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.pending_tools = sample_pending_tools();
    harness.app.state.tool_phase = ToolPhase::Deciding;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ApproveTool {
            call_id: String::from("call-2"),
            result: Ok(()),
        });
    assert_eq!(harness.app.state.pending_tools[1].approved, Some(true));
    assert_eq!(harness.app.state.tool_phase, ToolPhase::ExecutingBatch);
    let requests = harness.drain_requests();
    assert!(matches!(
        requests.as_slice(),
        [RuntimeRequest::ExecuteApprovedTools { session_id }] if session_id == "sess-2"
    ));
}
#[test]
fn switching_sessions_clears_foreground_tool_state_and_requests_fresh_data() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-1"));
    harness.app.state.pending_tools = sample_pending_tools();
    harness.app.state.retry_waiting = true;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::LoadSession {
            session_id: String::from("sess-2"),
            result: Ok(true),
        });
    assert_eq!(
        harness.app.state.current_session_id.as_deref(),
        Some("sess-2")
    );
    assert!(harness.app.state.pending_tools.is_empty());
    assert!(!harness.app.state.retry_waiting);
    let requests = harness.drain_requests();
    assert!(requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::GetCurrentTip { session_id } if session_id == "sess-2")));
    assert!(requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::GetChatHistory { session_id } if session_id == "sess-2")));
    assert!(requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::GetPendingTools { session_id } if session_id == "sess-2")));
}
#[test]
fn failed_tool_result_for_current_session_adds_optimistic_tool_message() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.pending_tools = sample_pending_tools();
    harness.app.handle_runtime_event(Event::ToolResultReady {
        session_id: String::from("sess-2"),
        call_id: String::from("call-2"),
        tool_id: String::from("write_file"),
        success: false,
        output: String::from("{\"error\":\"boom\"}"),
        denied: false,
    });
    assert_eq!(harness.app.state.pending_tools.len(), 1);
    assert_eq!(harness.app.state.optimistic_tool_messages.len(), 1);
    assert!(
        harness.app.state.optimistic_tool_messages[0]
            .content
            .contains("\"error\": \"boom\"")
    );
}
#[test]
fn tool_result_for_background_session_does_not_cache_tool_message() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.pending_tools = sample_pending_tools();
    harness.app.handle_runtime_event(Event::ToolResultReady {
        session_id: String::from("sess-1"),
        call_id: String::from("call-bg"),
        tool_id: String::from("write_file"),
        success: false,
        output: String::from("{\"error\":\"boom\"}"),
        denied: false,
    });
    assert!(harness.app.state.optimistic_tool_messages.is_empty());
    let requests = harness.drain_requests();
    assert!(
        requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::ListSessions))
    );
}
#[test]
fn chat_history_response_reconciles_matching_optimistic_tool_messages() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-1"));
    harness
        .app
        .state
        .optimistic_tool_messages
        .push(OptimisticToolMessage {
            local_id: String::from("local-tool-1"),
            content: String::from("Tool 'write_file' result:\n{\n  \"error\": \"boom\"\n}"),
        });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ChatHistory {
            session_id: String::from("sess-1"),
            result: Ok(BTreeMap::from([(
                MessageId::new("m1"),
                message(
                    "m1",
                    None,
                    ChatRole::Tool,
                    "Tool 'write_file' result:\n{\n  \"error\": \"boom\"\n}",
                ),
            )])),
        });
    assert!(harness.app.state.optimistic_tool_messages.is_empty());
}
#[test]
fn tool_call_event_adds_pending_tool_and_status() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_runtime_event(Event::ToolCallDetected {
        session_id: String::from("sess-2"),
        call_id: String::from("call-3"),
        tool_id: String::from("read_file"),
        args: String::from("{\"path\":\"Cargo.toml\"}"),
        description: String::from("Read the workspace manifest"),
        risk_level: String::from("read_only_workspace"),
        reasons: vec![String::from("Reads local config")],
        queue_order: 0,
    });
    assert_eq!(harness.app.state.pending_tools.len(), 1);
    assert_eq!(harness.app.state.tool_phase, ToolPhase::Deciding);
    assert_eq!(harness.app.state.status, "1 tool call(s) pending");
}
#[test]
fn tool_call_for_background_session_only_refreshes_sessions() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.handle_runtime_event(Event::ToolCallDetected {
        session_id: String::from("sess-1"),
        call_id: String::from("call-3"),
        tool_id: String::from("read_file"),
        args: String::from("{\"path\":\"Cargo.toml\"}"),
        description: String::from("Read the workspace manifest"),
        risk_level: String::from("read_only_workspace"),
        reasons: vec![String::from("Reads local config")],
        queue_order: 0,
    });
    assert!(harness.app.state.pending_tools.is_empty());
    let requests = harness.drain_requests();
    assert!(
        requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::ListSessions))
    );
}
#[test]
fn stream_cancelled_event_refreshes_foreground_session_and_sessions_list() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("sess-2"));
    harness.app.state.is_streaming = true;
    harness.app.handle_runtime_event(Event::StreamCancelled {
        session_id: String::from("sess-2"),
        message_id: String::from("m-cancelled"),
    });
    assert!(!harness.app.state.is_streaming);
    assert_eq!(harness.app.state.status, "Stream cancelled");
    let requests = harness.drain_requests();
    assert!(requests.iter().any(|request| matches!(
        request,
        RuntimeRequest::GetCurrentTip { session_id } if session_id == "sess-2"
    )));
    assert!(requests.iter().any(|request| matches!(
        request,
        RuntimeRequest::GetChatHistory { session_id } if session_id == "sess-2"
    )));
    assert!(requests.iter().any(|request| matches!(
        request,
        RuntimeRequest::GetPendingTools { session_id } if session_id == "sess-2"
    )));
    assert!(
        requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::ListSessions))
    );
}
#[test]
fn sessions_menu_renders_streaming_suffix() {
    let state = AppState {
        mode: UiMode::SessionsMenu,
        sessions: sample_sessions(),
        ..AppState::default()
    };
    let snapshot = render_state_snapshot(&state, 80, 12);
    assert!(snapshot.contains("[streaming]"));
}
fn request_name(request: &RuntimeRequest) -> &'static str {
    match request {
        RuntimeRequest::ListModels => "ListModels",
        RuntimeRequest::ListAgentProfiles { .. } => "ListAgentProfiles",
        RuntimeRequest::ListProviderDefinitions => "ListProviderDefinitions",
        RuntimeRequest::GetSettings => "GetSettings",
        RuntimeRequest::GetOpenAiCodexAuthStatus => "GetOpenAiCodexAuthStatus",
        RuntimeRequest::StartOpenAiCodexBrowserLogin => "StartOpenAiCodexBrowserLogin",
        RuntimeRequest::StartOpenAiCodexDeviceCodeLogin => "StartOpenAiCodexDeviceCodeLogin",
        RuntimeRequest::CancelOpenAiCodexLogin => "CancelOpenAiCodexLogin",
        RuntimeRequest::LogoutOpenAiCodexAuth => "LogoutOpenAiCodexAuth",
        RuntimeRequest::CreateSession => "CreateSession",
        RuntimeRequest::SetSessionProfile { .. } => "SetSessionProfile",
        RuntimeRequest::SendMessage { .. } => "SendMessage",
        RuntimeRequest::SaveSettings { .. } => "SaveSettings",
        RuntimeRequest::GetChatHistory { .. } => "GetChatHistory",
        RuntimeRequest::GetSessionContextUsage { .. } => "GetSessionContextUsage",
        RuntimeRequest::GetCurrentTip { .. } => "GetCurrentTip",
        RuntimeRequest::UndoLastUserMessage { .. } => "UndoLastUserMessage",
        RuntimeRequest::GetPendingTools { .. } => "GetPendingTools",
        RuntimeRequest::LoadSession { .. } => "LoadSession",
        RuntimeRequest::ListSessions => "ListSessions",
        RuntimeRequest::DeleteSession { .. } => "DeleteSession",
        RuntimeRequest::ApproveTool { .. } => "ApproveTool",
        RuntimeRequest::DenyTool { .. } => "DenyTool",
        RuntimeRequest::CancelStream { .. } => "CancelStream",
        RuntimeRequest::ContinueSession { .. } => "ContinueSession",
        RuntimeRequest::ExecuteApprovedTools { .. } => "ExecuteApprovedTools",
    }
}
