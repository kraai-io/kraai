use std::collections::HashMap;
use std::io::Write;

use base64::Engine;
use color_eyre::eyre::Result;
use kraai_runtime::{FieldDefinition, ModelSettings, ProviderSettings};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use crate::components::{ChatHistory, TextInput, VisibleChatView};

use super::{
    ActiveSettingsEditor, AppState, ChatCellPosition, ChatSelection, ProviderAuthState,
    ProvidersAdvancedFocus, ProvidersView, SettingsModelField, SettingsProviderField,
    ToolApprovalAction, ToolPhase, UiMode, field_value_display, flatten_models_map,
    provider_definition_rank,
};

pub(super) const STATUSLINE_STREAMING_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub(super) fn bottom_panel_height(state: &AppState, area: Rect) -> u16 {
    if state.mode == UiMode::Chat && state.tool_phase == ToolPhase::Deciding {
        10.min(area.height.saturating_sub(1).max(3))
    } else {
        TextInput::new(&state.input, state.input_cursor).get_height(area.width)
    }
}

impl Widget for &AppState {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let input_height = bottom_panel_height(self, area);
        let layout = Layout::vertical([
            Constraint::Min(area.height.saturating_sub(input_height + 1)),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .flex(Flex::End);
        let [chat_history_area, status_area, input_area] = layout.areas(area);

        self.refresh_chat_render_cache(chat_history_area.width);
        {
            let cache = self.chat_render_cache.borrow();
            ChatHistory::render_prebuilt_sections(
                &cache.sections,
                cache.total_lines,
                chat_history_area,
                buf,
                self.scroll,
                self.auto_scroll,
            );
        }
        render_chat_selection_overlay(self.visible_chat_view.as_ref(), self.selection, buf);

        Paragraph::new(statusline_line(self))
            .style(Style::default().fg(Color::DarkGray))
            .render(status_area, buf);

        if self.mode == UiMode::Chat && self.tool_phase == ToolPhase::Deciding {
            render_tool_approval_panel(self, input_area, buf);
        } else {
            TextInput::new(&self.input, self.input_cursor).render(input_area, buf);
        }
        if self.mode == UiMode::Chat && self.tool_phase != ToolPhase::Deciding {
            render_command_popup(self, area, input_area, buf);
        }

        match self.mode {
            UiMode::AgentMenu => render_agent_menu(self, area, buf),
            UiMode::ModelMenu => render_model_menu(self, area, buf),
            UiMode::ProvidersMenu => render_providers_menu(self, area, buf),
            UiMode::SessionsMenu => render_sessions_menu(self, area, buf),
            UiMode::Help => render_help_menu(area, buf),
            UiMode::Chat => {}
        }
    }
}

fn statusline_line(state: &AppState) -> Line<'static> {
    let separator = Span::styled(" · ", Style::default().fg(Color::DarkGray));
    let mut spans = vec![
        Span::styled(
            statusline_activity_label(state),
            Style::default().fg(statusline_activity_color(state)),
        ),
        separator.clone(),
        Span::raw(statusline_model_label(state)),
        separator.clone(),
        Span::raw(statusline_agent_label(state)),
    ];

    spans.push(separator.clone());
    spans.push(Span::raw(statusline_context_label(state)));

    spans.push(separator);
    spans.push(Span::raw(state.status.clone()));
    Line::from(spans)
}

fn statusline_activity_label(state: &AppState) -> String {
    if state.runtime_is_active() {
        return STATUSLINE_STREAMING_FRAMES
            [state.statusline_animation_frame % STATUSLINE_STREAMING_FRAMES.len()]
        .to_string();
    }

    if state.status == "Stream cancelled" {
        return String::from("cancelled");
    }

    String::from("idle")
}

fn statusline_activity_color(state: &AppState) -> Color {
    if state.runtime_is_active() {
        Color::Cyan
    } else if state.status == "Stream cancelled" {
        Color::Yellow
    } else {
        Color::DarkGray
    }
}

fn statusline_model_label(state: &AppState) -> String {
    let Some(provider_id) = state.selected_provider_id.as_deref() else {
        return String::from("none");
    };
    let Some(model_id) = state.selected_model_id.as_deref() else {
        return String::from("none");
    };

    let model_name = state
        .models_by_provider
        .get(provider_id)
        .and_then(|models| models.iter().find(|model| model.id == model_id))
        .map(|model| model.name.as_str())
        .unwrap_or(model_id);

    format!("{provider_id}/{model_name}")
}

fn statusline_agent_label(state: &AppState) -> String {
    let Some(profile_id) = state.selected_profile_id.as_deref() else {
        return String::from("none");
    };

    state
        .agent_profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .map(|profile| profile.display_name.clone())
        .unwrap_or_else(|| profile_id.to_string())
}

fn statusline_context_label(state: &AppState) -> String {
    format_context_label(
        state
            .context_usage
            .as_ref()
            .map(|usage| usage.used_context_tokens()),
        state
            .context_usage
            .as_ref()
            .and_then(|usage| usage.max_context)
            .or_else(|| selected_model_max_context(state)),
    )
}

fn selected_model_max_context(state: &AppState) -> Option<usize> {
    let provider_id = state.selected_provider_id.as_deref()?;
    let model_id = state.selected_model_id.as_deref()?;

    state
        .models_by_provider
        .get(provider_id)?
        .iter()
        .find(|model| model.id == model_id)
        .and_then(|model| model.max_context)
}

pub(super) fn format_token_count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index != 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

pub(super) fn format_context_label(
    used_context_tokens: Option<usize>,
    max_context: Option<usize>,
) -> String {
    let used_context_tokens = used_context_tokens.unwrap_or_default();
    let used = format_token_count(used_context_tokens);

    match max_context {
        Some(max_context) if max_context > 0 => format!(
            "ctx {used}/{} ({}%)",
            format_token_count(max_context),
            used_context_tokens
                .saturating_mul(100)
                .checked_div(max_context)
                .unwrap_or_default()
        ),
        _ => format!("ctx {used}"),
    }
}

pub(super) fn render_chat_selection_overlay(
    visible_chat_view: Option<&VisibleChatView>,
    selection: Option<ChatSelection>,
    buf: &mut Buffer,
) {
    let (Some(view), Some(selection)) = (visible_chat_view, selection) else {
        return;
    };
    let Some((start, end)) = normalized_selection_range(view, selection) else {
        return;
    };

    for line_index in start.line..=end.line {
        let Some(line) = view.lines.get(line_index) else {
            continue;
        };
        let line_width = line.text.chars().count();
        if line_width == 0 {
            continue;
        }

        let start_col = if line_index == start.line {
            start.column.min(line_width.saturating_sub(1))
        } else {
            0
        };
        let end_col = if line_index == end.line {
            end.column.min(line_width.saturating_sub(1))
        } else {
            line_width.saturating_sub(1)
        };

        for column in start_col..=end_col {
            let x = view.area.x + column as u16;
            let y = line.y;
            let cell = &mut buf[(x, y)];
            cell.set_fg(Color::Black);
            cell.set_bg(Color::Cyan);
        }
    }
}

fn normalized_selection_range(
    view: &VisibleChatView,
    selection: ChatSelection,
) -> Option<(ChatCellPosition, ChatCellPosition)> {
    let (mut start, mut end) = selection.normalized();
    let start_width = view.lines.get(start.line)?.text.chars().count();
    let end_width = view.lines.get(end.line)?.text.chars().count();

    if start_width > 0 {
        start.column = start.column.min(start_width.saturating_sub(1));
    } else {
        start.column = 0;
    }

    if end_width > 0 {
        end.column = end.column.min(end_width.saturating_sub(1));
    } else {
        end.column = 0;
    }

    Some((start, end))
}

pub(super) fn selection_text(view: &VisibleChatView, selection: ChatSelection) -> Option<String> {
    let (start, end) = normalized_selection_range(view, selection)?;
    let mut selected_lines = Vec::new();

    for line_index in start.line..=end.line {
        let line = view.lines.get(line_index)?;
        let chars: Vec<char> = line.text.chars().collect();
        let line_width = chars.len();

        let text = if line_width == 0 {
            String::new()
        } else {
            let start_col = if line_index == start.line {
                start.column.min(line_width.saturating_sub(1))
            } else {
                0
            };
            let end_col = if line_index == end.line {
                end.column.min(line_width.saturating_sub(1))
            } else {
                line_width.saturating_sub(1)
            };
            chars[start_col..=end_col].iter().collect()
        };

        selected_lines.push(text);
    }

    Some(selected_lines.join("\n"))
}

pub(super) fn is_copy_shortcut(key_event: KeyEvent) -> bool {
    match key_event.code {
        KeyCode::Char(c) => {
            key_event.modifiers.contains(KeyModifiers::CONTROL)
                && (key_event.modifiers.contains(KeyModifiers::SHIFT) || c.is_ascii_uppercase())
                && c.eq_ignore_ascii_case(&'c')
        }
        _ => false,
    }
}

pub(super) fn copy_via_osc52(text: &str) -> Result<(), String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let sequence = format!("\x1b]52;c;{encoded}\x07");
    let mut stdout = std::io::stdout();
    stdout
        .write_all(sequence.as_bytes())
        .map_err(|err| format!("stdout write failed: {err}"))?;
    stdout
        .flush()
        .map_err(|err| format!("stdout flush failed: {err}"))
}

pub(super) fn active_command_prefix(input: &str) -> Option<&str> {
    let cmd = input.strip_prefix('/')?;
    if cmd.chars().any(char::is_whitespace) {
        return None;
    }
    Some(cmd)
}

pub(super) fn is_known_slash_command(command_line: &str) -> bool {
    command_line
        .split_whitespace()
        .next()
        .is_some_and(|command| {
            super::SLASH_COMMANDS
                .iter()
                .any(|(known, _)| *known == command)
        })
}

pub(super) fn slash_command_matches(prefix: &str) -> Vec<(&'static str, &'static str)> {
    super::SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|(command, _)| command.starts_with(prefix))
        .collect()
}

pub(super) fn adjust_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        (current + delta as usize).min(len - 1)
    }
}

fn settings_provider_field_label(state: &AppState, field: &SettingsProviderField) -> String {
    match field {
        SettingsProviderField::Id => String::from("Provider ID"),
        SettingsProviderField::TypeId => String::from("Provider Type"),
        SettingsProviderField::Value(key) => state
            .settings_provider_field_definition(key)
            .map(field_label)
            .unwrap_or_else(|| key.clone()),
    }
}

fn field_label(field: &FieldDefinition) -> String {
    field.label.clone()
}

fn settings_provider_field_value(
    state: &AppState,
    provider: &ProviderSettings,
    field: &SettingsProviderField,
) -> String {
    match field {
        SettingsProviderField::Id => provider.id.clone(),
        SettingsProviderField::TypeId => state
            .settings_current_provider_definition()
            .map(|definition| definition.display_name.clone())
            .unwrap_or_else(|| provider.type_id.clone()),
        SettingsProviderField::Value(key) => field_value_display(&provider.values, key),
    }
}

fn settings_model_field_label(state: &AppState, field: &SettingsModelField) -> String {
    match field {
        SettingsModelField::Id => String::from("Model ID"),
        SettingsModelField::Value(key) => state
            .settings_model_field_definition(key)
            .map(field_label)
            .unwrap_or_else(|| key.clone()),
    }
}

trait SettingsUiStateExt {
    fn settings_current_provider_definition(&self) -> Option<&kraai_runtime::ProviderDefinition>;
    fn settings_provider_field_definition(&self, key: &str) -> Option<&FieldDefinition>;
    fn settings_model_field_definition(&self, key: &str) -> Option<&FieldDefinition>;
    fn settings_current_provider_fields(&self) -> Vec<SettingsProviderField>;
    fn settings_current_model_fields(&self) -> Vec<SettingsModelField>;
}

impl SettingsUiStateExt for AppState {
    fn settings_current_provider_definition(&self) -> Option<&kraai_runtime::ProviderDefinition> {
        let provider = self
            .settings_draft
            .as_ref()
            .and_then(|draft| draft.providers.get(self.settings_provider_index))?;
        self.provider_definitions
            .iter()
            .find(|definition| definition.type_id == provider.type_id)
    }

    fn settings_provider_field_definition(&self, key: &str) -> Option<&FieldDefinition> {
        self.settings_current_provider_definition()?
            .provider_fields
            .iter()
            .find(|field| field.key == key)
    }

    fn settings_model_field_definition(&self, key: &str) -> Option<&FieldDefinition> {
        self.settings_current_provider_definition()?
            .model_fields
            .iter()
            .find(|field| field.key == key)
    }

    fn settings_current_provider_fields(&self) -> Vec<SettingsProviderField> {
        let mut fields = vec![SettingsProviderField::Id, SettingsProviderField::TypeId];
        if let Some(definition) = self.settings_current_provider_definition() {
            fields.extend(
                definition
                    .provider_fields
                    .iter()
                    .map(|field| SettingsProviderField::Value(field.key.clone())),
            );
        }
        fields
    }

    fn settings_current_model_fields(&self) -> Vec<SettingsModelField> {
        let mut fields = vec![SettingsModelField::Id];
        if let Some(definition) = self.settings_current_provider_definition() {
            fields.extend(
                definition
                    .model_fields
                    .iter()
                    .map(|field| SettingsModelField::Value(field.key.clone())),
            );
        }
        fields
    }
}

fn settings_model_field_value(model: &ModelSettings, field: &SettingsModelField) -> String {
    match field {
        SettingsModelField::Id => model.id.clone(),
        SettingsModelField::Value(key) => field_value_display(&model.values, key),
    }
}

pub(super) fn parse_settings_errors(message: &str) -> HashMap<String, String> {
    let mut errors = HashMap::new();
    for line in message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some((field, error)) = line.split_once(": ") {
            errors.insert(field.to_string(), error.to_string());
        }
    }
    errors
}

fn render_command_popup(state: &AppState, area: Rect, input_area: Rect, buf: &mut Buffer) {
    if state.command_popup_dismissed {
        return;
    }
    let Some(prefix) = active_command_prefix(&state.input) else {
        return;
    };
    let matches = slash_command_matches(prefix);
    if matches.is_empty() {
        return;
    }

    let visible_count = matches.len().min(6);
    let popup_height = (visible_count as u16).saturating_add(2);
    let popup_width = area.width.saturating_mul(3) / 5;
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect::new(
        area.x + 1,
        popup_y,
        popup_width.max(24),
        popup_height.max(3),
    );

    let selected_idx = if state.command_completion_prefix.as_deref() == Some(prefix) {
        state
            .command_completion_index
            .min(matches.len().saturating_sub(1))
    } else {
        0
    };
    let visible_lines = popup_area.height.saturating_sub(2) as usize;
    let scroll_offset = menu_scroll_offset(selected_idx, matches.len(), visible_lines);

    let mut lines = Vec::new();
    for (idx, (command, description)) in matches
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_count)
    {
        let selected = idx == selected_idx;
        let marker = if selected { ">" } else { " " };
        lines.push(Line::styled(
            format!("{marker} /{command:<9} {description}"),
            if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            },
        ));
    }

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("Command (Tab/Down next, Shift-Tab/Up prev, Enter run)")
                .borders(Borders::ALL),
        )
        .render(popup_area, buf);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let popup = Layout::vertical([
        Constraint::Length((area.height.saturating_sub(height)) / 2),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .split(area)[1];

    Layout::horizontal([
        Constraint::Length((area.width.saturating_sub(width)) / 2),
        Constraint::Length(width),
        Constraint::Min(0),
    ])
    .split(popup)[1]
}

fn render_model_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let models = flatten_models_map(&state.models_by_provider);
    let popup_area = centered_rect(area.width.saturating_mul(3) / 4, area.height / 2, area);

    let mut lines = vec![Line::styled(
        "Select model (Enter to choose, Esc to close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if models.is_empty() {
        lines.push(Line::raw("No models available"));
    } else {
        for (idx, (provider, model)) in models.iter().enumerate() {
            let selected = idx == state.model_menu_index;
            let marker = if selected { ">" } else { " " };
            let current = state
                .selected_provider_id
                .as_ref()
                .zip(state.selected_model_id.as_ref())
                .is_some_and(|(p, m)| p == provider && m == &model.id);
            let suffix = if current { " (current)" } else { "" };
            lines.push(Line::styled(
                format!("{marker} {provider} / {}{}", model.name, suffix),
                if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                },
            ));
        }
    }

    let visible_lines = popup_area.height.saturating_sub(2) as usize;
    let selected_line = if models.is_empty() {
        1
    } else {
        state.model_menu_index.saturating_add(1)
    };
    let scroll_offset = menu_scroll_offset(selected_line, lines.len(), visible_lines);

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/model").borders(Borders::ALL))
        .scroll((scroll_offset as u16, 0))
        .render(popup_area, buf);
}

fn render_agent_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(area.width.saturating_mul(3) / 4, area.height / 2, area);

    let mut lines = vec![Line::styled(
        "Select agent (Enter to choose, Esc to close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if state.agent_profiles.is_empty() {
        lines.push(Line::raw("No agents available"));
    } else {
        for (idx, profile) in state.agent_profiles.iter().enumerate() {
            let selected = idx == state.agent_menu_index;
            let marker = if selected { ">" } else { " " };
            let current = state
                .selected_profile_id
                .as_ref()
                .is_some_and(|profile_id| profile_id == &profile.id);
            let suffix = if current { " (current)" } else { "" };
            lines.push(Line::styled(
                format!("{marker} {}{}", profile.id, suffix),
                if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                },
            ));
            lines.push(Line::raw(format!(
                "  {} | risk={} | source={}",
                profile.description,
                profile.default_risk_level.as_str(),
                match profile.source {
                    kraai_runtime::AgentProfileSource::BuiltIn => "built-in",
                    kraai_runtime::AgentProfileSource::Global => "global",
                    kraai_runtime::AgentProfileSource::Workspace => "workspace",
                }
            )));
        }
    }

    if let Some(warning) = state.agent_profile_warnings.first() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("Warning: {}", warning.message),
            Style::default().fg(Color::Yellow),
        ));
    }

    let visible_lines = popup_area.height.saturating_sub(2) as usize;
    let selected_line = if state.agent_profiles.is_empty() {
        1
    } else {
        state.agent_menu_index.saturating_mul(2).saturating_add(1)
    };
    let scroll_offset = menu_scroll_offset(selected_line, lines.len(), visible_lines);

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/agent").borders(Borders::ALL))
        .scroll((scroll_offset as u16, 0))
        .render(popup_area, buf);
}

pub(super) fn menu_scroll_offset(
    selected_line: usize,
    total_lines: usize,
    visible_lines: usize,
) -> usize {
    if visible_lines == 0 || total_lines <= visible_lines {
        return 0;
    }

    let max_scroll = total_lines - visible_lines;
    selected_line
        .saturating_sub(visible_lines.saturating_sub(1))
        .min(max_scroll)
}

pub(super) fn model_menu_next_index(current_index: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }

    (current_index + 1) % len
}

pub(super) fn model_menu_previous_index(current_index: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }

    (current_index + len - 1) % len
}

fn render_providers_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(
        area.width.saturating_mul(11) / 12,
        area.height.saturating_mul(4) / 5,
        area,
    );

    Clear.render(popup_area, buf);
    let outer = Block::default().title("/providers").borders(Borders::ALL);
    let inner = outer.inner(popup_area);
    outer.render(popup_area, buf);

    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .areas(inner);

    let header = providers_header_lines(state);
    Paragraph::new(Text::from(header)).render(header_area, buf);

    match state.providers_view {
        ProvidersView::List => render_provider_list_view(state, body_area, buf),
        ProvidersView::Connect => render_connect_provider_view(state, body_area, buf),
        ProvidersView::Detail => render_provider_detail_view(state, body_area, buf),
        ProvidersView::Advanced => render_provider_advanced_view(state, body_area, buf),
    }

    let footer_text = providers_footer_text(state);
    Paragraph::new(Line::raw(footer_text))
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL))
        .render(footer_area, buf);
}

fn providers_header_lines(state: &AppState) -> Vec<Line<'static>> {
    if let Some(editor) = &state.settings_editor {
        let target = match editor {
            ActiveSettingsEditor::Provider(field) => settings_provider_field_label(state, field),
            ActiveSettingsEditor::Model(field) => settings_model_field_label(state, field),
        };
        return vec![
            Line::styled("Providers", Style::default().add_modifier(Modifier::BOLD)),
            Line::raw(format!("Editing {target}: {}", state.settings_editor_input)),
        ];
    }

    match state.providers_view {
        ProvidersView::List => vec![
            Line::styled("Providers", Style::default().add_modifier(Modifier::BOLD)),
            Line::raw("Enter=open, a=connect, d=delete, r=refresh, Esc=close"),
        ],
        ProvidersView::Connect => vec![
            Line::styled(
                "Connect a provider",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw(format!("Search: {}", state.connect_provider_search)),
        ],
        ProvidersView::Detail => {
            if let Some(provider) = state
                .settings_draft
                .as_ref()
                .and_then(|draft| draft.providers.get(state.settings_provider_index))
            {
                let display_name = provider_display_name(state, &provider.type_id);
                vec![
                    Line::styled(
                        format!("Provider: id={}", provider.id),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!(
                        "{display_name}  type={}  b/c/x/l/e/r shortcuts, y=copy, o=open, Esc=back",
                        provider.type_id
                    )),
                ]
            } else {
                vec![
                    Line::styled(
                        "Provider detail",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Line::raw("b/c/x/l/e/r shortcuts, y=copy, o=open, Esc=back"),
                ]
            }
        }
        ProvidersView::Advanced => vec![
            Line::styled(
                "Advanced config",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw("Tab=switch section, Enter=edit, a=add, d=delete, Esc=back"),
        ],
    }
}

fn providers_footer_text(state: &AppState) -> String {
    if state.settings_delete_armed {
        return String::from("Delete armed: press d again to confirm");
    }

    if let Some(editor) = &state.settings_editor {
        let field = match editor {
            ActiveSettingsEditor::Provider(field) => settings_provider_field_label(state, field),
            ActiveSettingsEditor::Model(field) => settings_model_field_label(state, field),
        };
        return format!("Editing {field}: Enter=commit, Esc=cancel");
    }

    match state.providers_view {
        ProvidersView::List => String::from("One provider panel at a time"),
        ProvidersView::Connect => String::from("Flat list. Type to filter."),
        ProvidersView::Detail => state.status.clone(),
        ProvidersView::Advanced => {
            String::from("Advanced config edits provider fields and model overrides")
        }
    }
}

fn render_provider_list_view(state: &AppState, area: Rect, buf: &mut Buffer) {
    let mut lines = vec![Line::styled(
        "Configured providers",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    match &state.settings_draft {
        Some(draft) if !draft.providers.is_empty() => {
            for (idx, provider) in draft.providers.iter().enumerate() {
                let selected = idx == state.settings_provider_index;
                let marker = if selected { ">" } else { " " };
                let display_name = provider_display_name(state, &provider.type_id);
                let auth_badge = if provider.type_id == "openai-codex" {
                    format!(" [{}]", openai_auth_badge(state))
                } else {
                    String::new()
                };
                let model_count = state
                    .models_by_provider
                    .get(&provider.id)
                    .map_or(0, Vec::len);
                lines.push(Line::styled(
                    format!(
                        "{marker} id={}  {display_name}  type={}{}  models={model_count}",
                        provider.id, provider.type_id, auth_badge
                    ),
                    selection_style(selected),
                ));
            }
        }
        Some(_) => lines.push(Line::raw("No providers configured")),
        None => lines.push(Line::raw("Loading providers...")),
    }

    Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

fn render_connect_provider_view(state: &AppState, area: Rect, buf: &mut Buffer) {
    let modal_area = centered_rect(
        area.width.saturating_mul(4) / 5,
        area.height.saturating_mul(5) / 6,
        area,
    );
    let visible_lines = modal_area.height.saturating_sub(4) as usize;
    let definitions = filtered_provider_definitions(state);
    let selected_idx = state
        .connect_provider_index
        .min(definitions.len().saturating_sub(1));
    let scroll_offset = menu_scroll_offset(
        selected_idx,
        definitions.len().saturating_add(1),
        visible_lines,
    );

    let mut lines = vec![Line::raw(format!(
        "Search {}",
        state.connect_provider_search
    ))];
    if definitions.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::raw("No providers match the search"));
    } else {
        lines.push(Line::raw(""));
        for (idx, definition) in definitions.iter().enumerate() {
            let selected = idx == selected_idx;
            let marker = if selected { ">" } else { " " };
            lines.push(Line::styled(
                format!("{marker} {}", definition.display_name),
                if selected {
                    Style::default().fg(Color::Black).bg(Color::LightYellow)
                } else {
                    Style::default()
                },
            ));
            lines.push(Line::styled(
                format!("  {}", definition.description),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    Clear.render(modal_area, buf);
    Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("Connect a provider")
                .borders(Borders::ALL),
        )
        .scroll((scroll_offset as u16, 0))
        .wrap(Wrap { trim: false })
        .render(modal_area, buf);
}

fn render_provider_detail_view(state: &AppState, area: Rect, buf: &mut Buffer) {
    if let Some(provider) = state
        .settings_draft
        .as_ref()
        .and_then(|draft| draft.providers.get(state.settings_provider_index))
    {
        let display_name = provider_display_name(state, &provider.type_id);
        let actions_height = if provider.type_id == "openai-codex"
            && matches!(
                state.openai_codex_auth.state,
                ProviderAuthState::BrowserPending | ProviderAuthState::DeviceCodePending
            ) {
            7
        } else {
            5
        };
        let [details_area, actions_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(actions_height)]).areas(area);

        let model_count = state
            .models_by_provider
            .get(&provider.id)
            .map_or(0, Vec::len);

        if provider.type_id == "openai-codex" {
            let mut details_lines = vec![
                Line::raw(format!("State: {}", openai_auth_badge(state))),
                Line::raw(format!(
                    "Plan: {}",
                    state
                        .openai_codex_auth
                        .plan_type
                        .as_deref()
                        .unwrap_or("unknown")
                )),
                Line::raw(format!(
                    "Last refresh: {}",
                    state
                        .openai_codex_auth
                        .last_refresh
                        .as_deref()
                        .unwrap_or("never")
                )),
                Line::raw(format!("Models: {model_count}")),
            ];
            if let Some(error) = &state.openai_codex_auth.error {
                details_lines.push(Line::styled(
                    format!("Error: {error}"),
                    Style::default().fg(Color::Yellow),
                ));
            }
            match state.openai_codex_auth.state {
                ProviderAuthState::BrowserPending => {}
                ProviderAuthState::DeviceCodePending => {
                    if let Some(code) = &state.openai_codex_auth.user_code {
                        details_lines.push(Line::raw(format!("Code: {code}")));
                    }
                }
                ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => {}
            }

            Paragraph::new(Text::from(details_lines))
                .block(Block::default().title("OpenAI auth").borders(Borders::ALL))
                .wrap(Wrap { trim: false })
                .render(details_area, buf);
        } else {
            let details_lines = vec![
                Line::raw(format!("Name: {display_name}")),
                Line::raw(format!("Type: {}", provider.type_id)),
                Line::raw(format!("Models: {model_count}")),
                Line::raw(format!("Configured fields: {}", provider.values.len())),
            ];
            Paragraph::new(Text::from(details_lines))
                .block(Block::default().title("Details").borders(Borders::ALL))
                .wrap(Wrap { trim: false })
                .render(details_area, buf);
        }

        let shortcut_lines = if provider.type_id == "openai-codex" {
            let mut lines = vec![
                Line::styled("Shortcuts", Style::default().add_modifier(Modifier::BOLD)),
                Line::raw("b browser sign-in  c device code  x cancel  l logout"),
                Line::raw("e advanced config  r refresh models"),
            ];
            match state.openai_codex_auth.state {
                ProviderAuthState::BrowserPending => {
                    lines.push(Line::raw("Browser should open automatically."));
                    lines.push(Line::raw("y copy sign-in URL  o open again  x cancel"));
                }
                ProviderAuthState::DeviceCodePending => {
                    lines.push(Line::raw(
                        "Browser should open the verification page automatically.",
                    ));
                    lines.push(Line::raw("y copy device code  o open again  x cancel"));
                }
                ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => {}
            }
            lines
        } else {
            vec![
                Line::styled("Shortcuts", Style::default().add_modifier(Modifier::BOLD)),
                Line::raw("e advanced config  r refresh models"),
            ]
        };
        Paragraph::new(Text::from(shortcut_lines))
            .block(Block::default().title("Actions").borders(Borders::ALL))
            .wrap(Wrap { trim: false })
            .render(actions_area, buf);
    } else {
        Paragraph::new(Line::raw("No provider selected"))
            .block(Block::default().borders(Borders::ALL))
            .render(area, buf);
    }
}

fn render_provider_advanced_view(state: &AppState, area: Rect, buf: &mut Buffer) {
    let [summary_area, editor_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);

    let summary = match state.providers_advanced_focus {
        ProvidersAdvancedFocus::ProviderFields => "Section: provider fields",
        ProvidersAdvancedFocus::Models => "Section: models",
        ProvidersAdvancedFocus::ModelFields => "Section: model fields",
    };
    Paragraph::new(Line::raw(summary))
        .block(Block::default().borders(Borders::ALL))
        .render(summary_area, buf);

    match state.providers_advanced_focus {
        ProvidersAdvancedFocus::ProviderFields => {
            let mut lines = vec![Line::styled("Provider fields", pane_style(true))];
            if let Some(provider) = state
                .settings_draft
                .as_ref()
                .and_then(|draft| draft.providers.get(state.settings_provider_index))
            {
                let fields = state.settings_current_provider_fields();
                for (idx, field) in fields.iter().enumerate() {
                    let selected = idx == state.settings_provider_field_index;
                    let error_key = match field {
                        SettingsProviderField::Id => {
                            format!("providers[{}].id", state.settings_provider_index)
                        }
                        SettingsProviderField::TypeId => {
                            format!("providers[{}].type_id", state.settings_provider_index)
                        }
                        SettingsProviderField::Value(key) => {
                            format!("providers[{}].{}", state.settings_provider_index, key)
                        }
                    };
                    let mut line = format!(
                        "{} {:<18} {}",
                        if selected { ">" } else { " " },
                        settings_provider_field_label(state, field),
                        settings_provider_field_value(state, provider, field)
                    );
                    if let Some(error) = state.settings_errors.get(&error_key) {
                        line.push_str(&format!("  ! {error}"));
                    }
                    lines.push(Line::styled(line, selection_style(selected)));
                }
            } else {
                lines.push(Line::raw("No provider selected"));
            }
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: false })
                .render(editor_area, buf);
        }
        ProvidersAdvancedFocus::Models => {
            let mut lines = vec![Line::styled("Models", pane_style(true))];
            let model_indices = current_model_indices(state);
            if model_indices.is_empty() {
                lines.push(Line::raw("No models"));
            } else if let Some(draft) = &state.settings_draft {
                for (idx, model_index) in model_indices.iter().enumerate() {
                    if let Some(model) = draft.models.get(*model_index) {
                        let selected = idx == state.settings_model_index;
                        lines.push(Line::styled(
                            format!(
                                "{} {}",
                                if selected { ">" } else { " " },
                                if model.id.is_empty() {
                                    "<new model>"
                                } else {
                                    model.id.as_str()
                                }
                            ),
                            selection_style(selected),
                        ));
                    }
                }
            }
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL))
                .render(editor_area, buf);
        }
        ProvidersAdvancedFocus::ModelFields => {
            let mut lines = vec![Line::styled("Model fields", pane_style(true))];
            let model_indices = current_model_indices(state);
            if let Some(model_index) = model_indices.get(state.settings_model_index).copied() {
                if let Some(model) = state
                    .settings_draft
                    .as_ref()
                    .and_then(|draft| draft.models.get(model_index))
                {
                    let fields = state.settings_current_model_fields();
                    for (idx, field) in fields.iter().enumerate() {
                        let selected = idx == state.settings_model_field_index;
                        let error_key = match field {
                            SettingsModelField::Id => format!("models[{model_index}].id"),
                            SettingsModelField::Value(key) => {
                                format!("models[{model_index}].{key}")
                            }
                        };
                        let mut line = format!(
                            "{} {:<18} {}",
                            if selected { ">" } else { " " },
                            settings_model_field_label(state, field),
                            settings_model_field_value(model, field)
                        );
                        if let Some(error) = state.settings_errors.get(&error_key) {
                            line.push_str(&format!("  ! {error}"));
                        }
                        lines.push(Line::styled(line, selection_style(selected)));
                    }
                }
            } else {
                lines.push(Line::raw("No model selected"));
            }
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL))
                .wrap(Wrap { trim: false })
                .render(editor_area, buf);
        }
    }
}

fn filtered_provider_definitions(state: &AppState) -> Vec<&kraai_runtime::ProviderDefinition> {
    let query = state.connect_provider_search.trim().to_ascii_lowercase();
    let mut definitions: Vec<&kraai_runtime::ProviderDefinition> = state
        .provider_definitions
        .iter()
        .filter(|definition| {
            query.is_empty()
                || definition
                    .display_name
                    .to_ascii_lowercase()
                    .contains(&query)
                || definition.type_id.to_ascii_lowercase().contains(&query)
                || definition.description.to_ascii_lowercase().contains(&query)
        })
        .collect();
    definitions.sort_by(|left, right| {
        provider_definition_rank(left)
            .cmp(&provider_definition_rank(right))
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.type_id.cmp(&right.type_id))
    });
    definitions
}

fn current_model_indices(state: &AppState) -> Vec<usize> {
    let Some(provider) = state
        .settings_draft
        .as_ref()
        .and_then(|draft| draft.providers.get(state.settings_provider_index))
    else {
        return Vec::new();
    };

    state
        .settings_draft
        .as_ref()
        .map(|draft| {
            draft
                .models
                .iter()
                .enumerate()
                .filter_map(|(index, model)| (model.provider_id == provider.id).then_some(index))
                .collect()
        })
        .unwrap_or_default()
}

fn provider_display_name(state: &AppState, type_id: &str) -> String {
    state
        .provider_definitions
        .iter()
        .find(|definition| definition.type_id == type_id)
        .map(|definition| definition.display_name.clone())
        .unwrap_or_else(|| type_id.to_string())
}

fn openai_auth_badge(state: &AppState) -> String {
    match state.openai_codex_auth.state {
        ProviderAuthState::SignedOut => String::from("Signed out"),
        ProviderAuthState::BrowserPending => String::from("Signing in"),
        ProviderAuthState::DeviceCodePending => String::from("Awaiting code"),
        ProviderAuthState::Authenticated => state
            .openai_codex_auth
            .plan_type
            .clone()
            .unwrap_or_else(|| String::from("Signed in")),
    }
}

fn pane_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn selection_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn render_sessions_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(area.width.saturating_mul(4) / 5, area.height / 2, area);
    let visible_lines = popup_area.height.saturating_sub(2) as usize;
    let total_lines = state.sessions.len() + 2;
    let selected_line = state.sessions_menu_index.saturating_add(1);
    let scroll_offset = menu_scroll_offset(selected_line, total_lines, visible_lines);

    let mut lines = vec![Line::styled(
        "Sessions (Enter=load/new, x=delete, Esc=close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    let marker = if state.sessions_menu_index == 0 {
        ">"
    } else {
        " "
    };
    lines.push(Line::styled(
        format!("{marker} Start new chat"),
        if state.sessions_menu_index == 0 {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        },
    ));

    for (idx, session) in state.sessions.iter().enumerate() {
        let selected = state.sessions_menu_index == idx + 1;
        let marker = if selected { ">" } else { " " };
        let current = state
            .current_session_id
            .as_ref()
            .is_some_and(|sid| sid == &session.id);
        let title = session
            .title
            .clone()
            .unwrap_or_else(|| format!("Session {}", &session.id[..8.min(session.id.len())]));
        let current_suffix = if current { " (current)" } else { "" };
        let approval_suffix = if session.waiting_for_approval {
            " [approval]"
        } else {
            ""
        };
        let streaming_suffix = if session.is_streaming {
            " [streaming]"
        } else {
            ""
        };
        lines.push(Line::styled(
            format!("{marker} {title}{current_suffix}{approval_suffix}{streaming_suffix}"),
            if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            },
        ));
    }

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/sessions").borders(Borders::ALL))
        .scroll((scroll_offset as u16, 0))
        .render(popup_area, buf);
}

fn render_tool_approval_panel(state: &AppState, area: Rect, buf: &mut Buffer) {
    let Some(tool) = state
        .pending_tools
        .iter()
        .find(|tool| tool.approved.is_none())
    else {
        return;
    };

    let block = Block::default()
        .title(" Permission required ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    Clear.render(area, buf);
    block.render(area, buf);

    for y in area.y..area.y + area.height {
        let cell = &mut buf[(area.x, y)];
        cell.set_char(' ').set_bg(Color::Yellow);
    }

    let inner = area.inner(ratatui::layout::Margin {
        vertical: 1,
        horizontal: 2,
    });
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let footer_height = 1;
    let body_height = inner.height.saturating_sub(footer_height + 1);
    let [body_area, _spacer, footer_area] = Layout::vertical([
        Constraint::Length(body_height),
        Constraint::Length(inner.height.saturating_sub(body_height + footer_height)),
        Constraint::Length(footer_height),
    ])
    .areas(inner);

    let mut lines = vec![
        Line::styled(
            &tool.description,
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!("tool: {}  risk: {}", tool.tool_id, tool.risk_level),
            Style::default().fg(Color::Gray),
        ),
    ];

    for reason in &tool.reasons {
        lines.push(Line::styled(
            format!("why: {reason}"),
            Style::default().fg(Color::Gray),
        ));
    }

    lines.push(Line::raw(String::new()));
    lines.push(Line::styled("args", Style::default().fg(Color::Gray)));
    lines.push(Line::raw(tool.args.clone()));

    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .render(body_area, buf);

    let allow_style = if state.tool_approval_action == ToolApprovalAction::Allow {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };
    let reject_style = if state.tool_approval_action == ToolApprovalAction::Reject {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };

    let footer = Line::from(vec![
        Span::raw(" "),
        Span::styled("Allow", allow_style),
        Span::raw("   "),
        Span::styled("Reject", reject_style),
        Span::raw(" ".repeat(footer_area.width.saturating_sub(33) as usize)),
        Span::styled(
            "select <->  confirm Enter",
            Style::default().fg(Color::Gray),
        ),
    ]);

    Paragraph::new(footer)
        .style(Style::default().bg(Color::DarkGray))
        .render(footer_area, buf);
}

fn render_help_menu(area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(area.width.saturating_mul(3) / 5, area.height / 2, area);

    let lines = vec![
        Line::styled(
            "Slash Commands",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw("/agent     Open agent selector"),
        Line::raw("/continue  Reprompt the agent"),
        Line::raw("/help      Open this help menu"),
        Line::raw("/model     Open model selector"),
        Line::raw("/new       Start a new chat"),
        Line::raw("/providers Open providers"),
        Line::raw("/sessions  Open sessions menu"),
        Line::raw("/undo      Restore last user message"),
        Line::raw("/quit      Exit Kraai"),
        Line::raw(""),
        Line::styled(
            "Chat Navigation",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw("Enter       Send message"),
        Line::raw("Shift+Enter Add newline"),
        Line::raw("Up/Down    Scroll history"),
        Line::raw("PgUp/PgDn  Scroll faster"),
        Line::raw("End        Jump to latest"),
        Line::raw("Home       Jump to top"),
        Line::raw("Drag mouse Select chat text"),
        Line::raw("Ctrl+Shift+C Copy selection"),
        Line::raw(""),
        Line::raw("Esc closes menus."),
    ];

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/help").borders(Borders::ALL))
        .render(popup_area, buf);
}
