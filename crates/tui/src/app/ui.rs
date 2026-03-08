use std::collections::HashMap;
use std::io::Write;

use agent_runtime::{ModelSettings, ProviderSettings, ProviderType};
use base64::Engine;
use color_eyre::eyre::Result;
use ratatui::{
    buffer::Buffer,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::components::{ChatHistory, TextInput, VisibleChatView};

use super::{
    ActiveSettingsEditor, AppState, CachedMessageRender, ChatCellPosition, ChatSelection,
    SettingsFocus, SettingsModelField, SettingsProviderField, UiMode, flatten_models_map,
    message_fingerprint,
};

pub(super) const SETTINGS_MODEL_FIELDS: [SettingsModelField; 3] = [
    SettingsModelField::Id,
    SettingsModelField::Name,
    SettingsModelField::MaxContext,
];

impl Widget for &AppState {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let input_height = TextInput::new(&self.input, self.input_cursor).get_height(area.width);
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

        let selected_model = self
            .selected_provider_id
            .as_ref()
            .zip(self.selected_model_id.as_ref())
            .map(|(p, m)| format!("{p}/{m}"))
            .unwrap_or_else(|| String::from("none"));
        let stream_state = if self.is_streaming {
            "streaming"
        } else {
            "idle"
        };
        let pending_tools = self
            .pending_tools
            .iter()
            .filter(|tool| self.current_session_id.as_deref() == Some(tool.session_id.as_str()))
            .count();
        let status_line = format!(
            "{} | model={} | tools={} | {}",
            self.status, selected_model, pending_tools, stream_state
        );

        Paragraph::new(Line::raw(status_line))
            .style(Style::default().fg(Color::DarkGray))
            .render(status_area, buf);

        TextInput::new(&self.input, self.input_cursor).render(input_area, buf);
        if self.mode == UiMode::Chat {
            render_command_popup(self, area, input_area, buf);
        }

        match self.mode {
            UiMode::ModelMenu => render_model_menu(self, area, buf),
            UiMode::SettingsMenu => render_settings_menu(self, area, buf),
            UiMode::SessionsMenu => render_sessions_menu(self, area, buf),
            UiMode::ToolsMenu => render_tools_menu(self, area, buf),
            UiMode::Help => render_help_menu(area, buf),
            UiMode::Chat => {}
        }
    }
}

impl AppState {
    pub(super) fn refresh_chat_render_cache(&self, width: u16) {
        let needs_refresh = {
            let cache = self.chat_render_cache.borrow();
            cache.epoch != self.chat_epoch || cache.width != width
        };
        if !needs_refresh {
            return;
        }

        let rendered_messages = self.rendered_messages();
        let mut cache = self.chat_render_cache.borrow_mut();
        let mut prior_entries = std::mem::take(&mut cache.message_cache);
        if cache.width != width {
            prior_entries.clear();
        }

        let mut next_entries: HashMap<String, CachedMessageRender> = HashMap::new();
        let mut sections = Vec::new();
        let mut total_lines: u16 = 0;

        for msg in &rendered_messages {
            let key = msg.id.as_str().to_string();
            let fingerprint = message_fingerprint(msg);
            let lines = match prior_entries.remove(&key) {
                Some(entry) if entry.fingerprint == fingerprint => entry.lines,
                _ => std::sync::Arc::new(ChatHistory::build_message_lines(msg, width)),
            };

            if lines.is_empty() {
                continue;
            }

            if !sections.is_empty() {
                sections.push(std::sync::Arc::new(vec![ChatHistory::separator_line()]));
                total_lines = total_lines.saturating_add(1);
            }

            total_lines = total_lines.saturating_add(lines.len().min(u16::MAX as usize) as u16);
            sections.push(std::sync::Arc::clone(&lines));
            next_entries.insert(key, CachedMessageRender { fingerprint, lines });
        }

        cache.sections = sections;
        cache.total_lines = total_lines;
        cache.message_cache = next_entries;
        cache.width = width;
        cache.epoch = self.chat_epoch;
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

fn provider_type_label(provider_type: &ProviderType) -> &'static str {
    match provider_type {
        ProviderType::OpenAi => "OpenAI-compatible",
    }
}

fn settings_provider_field_label(field: SettingsProviderField) -> &'static str {
    match field {
        SettingsProviderField::Id => "Provider ID",
        SettingsProviderField::Type => "Provider Type",
        SettingsProviderField::BaseUrl => "Base URL",
        SettingsProviderField::ApiKey => "Inline API Key",
        SettingsProviderField::EnvVarApiKey => "Env Var",
        SettingsProviderField::OnlyListedModels => "Only Listed Models",
    }
}

fn settings_provider_field_value(
    provider: &ProviderSettings,
    field: SettingsProviderField,
) -> String {
    match field {
        SettingsProviderField::Id => provider.id.clone(),
        SettingsProviderField::Type => String::from(provider_type_label(&provider.provider_type)),
        SettingsProviderField::BaseUrl => provider.base_url.clone().unwrap_or_default(),
        SettingsProviderField::ApiKey => {
            if provider
                .api_key
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                String::from("••••••")
            } else {
                String::new()
            }
        }
        SettingsProviderField::EnvVarApiKey => provider.env_var_api_key.clone().unwrap_or_default(),
        SettingsProviderField::OnlyListedModels => {
            if provider.only_listed_models {
                String::from("yes")
            } else {
                String::from("no")
            }
        }
    }
}

fn settings_model_field_label(field: SettingsModelField) -> &'static str {
    match field {
        SettingsModelField::Id => "Model ID",
        SettingsModelField::Name => "Display Name",
        SettingsModelField::MaxContext => "Max Context",
    }
}

fn settings_model_field_value(model: &ModelSettings, field: SettingsModelField) -> String {
    match field {
        SettingsModelField::Id => model.id.clone(),
        SettingsModelField::Name => model.name.clone().unwrap_or_default(),
        SettingsModelField::MaxContext => model
            .max_context
            .map(|value| value.to_string())
            .unwrap_or_default(),
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

fn render_settings_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(
        area.width.saturating_mul(9) / 10,
        area.height.saturating_mul(4) / 5,
        area,
    );

    Clear.render(popup_area, buf);
    let outer = Block::default().title("/settings").borders(Borders::ALL);
    let inner = outer.inner(popup_area);
    outer.render(popup_area, buf);

    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .areas(inner);

    let header = if let Some(editor) = state.settings_editor {
        let target = match editor {
            ActiveSettingsEditor::Provider(field) => settings_provider_field_label(field),
            ActiveSettingsEditor::Model(field) => settings_model_field_label(field),
        };
        vec![
            Line::styled(
                "Settings Editor",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw(format!("Editing {target}: {}", state.settings_editor_input)),
        ]
    } else {
        vec![
            Line::styled(
                "Settings Editor",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw("Tab=next pane, Enter=edit/toggle, a=add, x=delete, s=save, Esc=close"),
        ]
    };
    Paragraph::new(Text::from(header)).render(header_area, buf);

    let [
        providers_area,
        provider_form_area,
        models_area,
        model_form_area,
    ] = Layout::horizontal([
        Constraint::Percentage(24),
        Constraint::Percentage(26),
        Constraint::Percentage(24),
        Constraint::Percentage(26),
    ])
    .areas(body_area);

    let mut provider_lines = vec![Line::styled(
        "Providers",
        pane_style(state.settings_focus == SettingsFocus::ProviderList),
    )];
    if let Some(draft) = &state.settings_draft {
        if draft.providers.is_empty() {
            provider_lines.push(Line::raw("No providers"));
        } else {
            for (idx, provider) in draft.providers.iter().enumerate() {
                let selected = idx == state.settings_provider_index;
                provider_lines.push(Line::styled(
                    format!(
                        "{} {}",
                        if selected { ">" } else { " " },
                        if provider.id.is_empty() {
                            "<new provider>"
                        } else {
                            provider.id.as_str()
                        }
                    ),
                    selection_style(
                        state.settings_focus == SettingsFocus::ProviderList && selected,
                    ),
                ));
            }
        }
    }
    Paragraph::new(Text::from(provider_lines))
        .block(Block::default().borders(Borders::ALL))
        .render(providers_area, buf);

    let mut provider_form_lines = vec![Line::styled(
        "Provider Fields",
        pane_style(state.settings_focus == SettingsFocus::ProviderForm),
    )];
    if let Some(provider) = state
        .settings_draft
        .as_ref()
        .and_then(|draft| draft.providers.get(state.settings_provider_index))
    {
        let fields = state
            .settings_draft
            .as_ref()
            .map(|_| match provider.provider_type {
                ProviderType::OpenAi => vec![
                    SettingsProviderField::Id,
                    SettingsProviderField::Type,
                    SettingsProviderField::BaseUrl,
                    SettingsProviderField::ApiKey,
                    SettingsProviderField::EnvVarApiKey,
                    SettingsProviderField::OnlyListedModels,
                ],
            })
            .unwrap_or_default();
        for (idx, field) in fields.iter().enumerate() {
            let selected = idx == state.settings_provider_field_index;
            let error_key = match field {
                SettingsProviderField::Id => {
                    format!("providers[{}].id", state.settings_provider_index)
                }
                SettingsProviderField::BaseUrl => {
                    format!("providers[{}].base_url", state.settings_provider_index)
                }
                SettingsProviderField::ApiKey | SettingsProviderField::EnvVarApiKey => {
                    format!("providers[{}].credentials", state.settings_provider_index)
                }
                SettingsProviderField::Type | SettingsProviderField::OnlyListedModels => {
                    String::new()
                }
            };
            let mut line = format!(
                "{} {:<18} {}",
                if selected { ">" } else { " " },
                settings_provider_field_label(*field),
                settings_provider_field_value(provider, *field)
            );
            if let Some(error) = state.settings_errors.get(&error_key)
                && !error_key.is_empty()
            {
                line.push_str(&format!("  ! {error}"));
            }
            provider_form_lines.push(Line::styled(
                line,
                selection_style(state.settings_focus == SettingsFocus::ProviderForm && selected),
            ));
        }
    } else {
        provider_form_lines.push(Line::raw("No provider selected"));
    }
    Paragraph::new(Text::from(provider_form_lines))
        .block(Block::default().borders(Borders::ALL))
        .render(provider_form_area, buf);

    let mut model_lines = vec![Line::styled(
        "Models",
        pane_style(state.settings_focus == SettingsFocus::ModelList),
    )];
    let model_indices = if let Some(provider) = state
        .settings_draft
        .as_ref()
        .and_then(|draft| draft.providers.get(state.settings_provider_index))
    {
        state
            .settings_draft
            .as_ref()
            .map(|draft| {
                draft
                    .models
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, model)| (model.provider_id == provider.id).then_some(idx))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    if model_indices.is_empty() {
        model_lines.push(Line::raw("No models"));
    } else if let Some(draft) = &state.settings_draft {
        for (idx, model_index) in model_indices.iter().enumerate() {
            if let Some(model) = draft.models.get(*model_index) {
                model_lines.push(Line::styled(
                    format!(
                        "{} {}",
                        if idx == state.settings_model_index {
                            ">"
                        } else {
                            " "
                        },
                        if model.id.is_empty() {
                            "<new model>"
                        } else {
                            model.id.as_str()
                        }
                    ),
                    selection_style(
                        state.settings_focus == SettingsFocus::ModelList
                            && idx == state.settings_model_index,
                    ),
                ));
            }
        }
    }
    Paragraph::new(Text::from(model_lines))
        .block(Block::default().borders(Borders::ALL))
        .render(models_area, buf);

    let mut model_form_lines = vec![Line::styled(
        "Model Fields",
        pane_style(state.settings_focus == SettingsFocus::ModelForm),
    )];
    if let Some(model_index) = model_indices.get(state.settings_model_index).copied() {
        if let Some(model) = state
            .settings_draft
            .as_ref()
            .and_then(|draft| draft.models.get(model_index))
        {
            for (idx, field) in SETTINGS_MODEL_FIELDS.iter().enumerate() {
                let selected = idx == state.settings_model_field_index;
                let error_key = match field {
                    SettingsModelField::Id => format!("models[{model_index}].id"),
                    SettingsModelField::Name => String::new(),
                    SettingsModelField::MaxContext => format!("models[{model_index}].max_context"),
                };
                let mut line = format!(
                    "{} {:<18} {}",
                    if selected { ">" } else { " " },
                    settings_model_field_label(*field),
                    settings_model_field_value(model, *field)
                );
                if let Some(error) = state.settings_errors.get(&error_key)
                    && !error_key.is_empty()
                {
                    line.push_str(&format!("  ! {error}"));
                }
                model_form_lines.push(Line::styled(
                    line,
                    selection_style(state.settings_focus == SettingsFocus::ModelForm && selected),
                ));
            }
        }
    } else {
        model_form_lines.push(Line::raw("No model selected"));
    }
    Paragraph::new(Text::from(model_form_lines))
        .block(Block::default().borders(Borders::ALL))
        .render(model_form_area, buf);

    let footer_text = if state.settings_delete_armed {
        String::from("Delete armed: press x again to confirm")
    } else if let Some(editor) = state.settings_editor {
        let field = match editor {
            ActiveSettingsEditor::Provider(field) => settings_provider_field_label(field),
            ActiveSettingsEditor::Model(field) => settings_model_field_label(field),
        };
        format!("Editing {field}: Enter=commit, Esc=cancel")
    } else {
        String::from("Providers and models are shared with the desktop app")
    };
    Paragraph::new(Line::raw(footer_text))
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL))
        .render(footer_area, buf);
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
        let suffix = if current { " (current)" } else { "" };
        lines.push(Line::styled(
            format!("{marker} {title}{suffix}"),
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

fn render_tools_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(
        area.width.saturating_mul(4) / 5,
        area.height.saturating_mul(2) / 3,
        area,
    );

    let mut lines = vec![Line::styled(
        "Tools (a=approve, d=deny, e=execute approved, Esc=close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    let current_tools: Vec<_> = state
        .pending_tools
        .iter()
        .filter(|tool| state.current_session_id.as_deref() == Some(tool.session_id.as_str()))
        .collect();

    if current_tools.is_empty() {
        lines.push(Line::raw("No pending tool calls"));
    } else {
        for (idx, tool) in current_tools.iter().enumerate() {
            let selected = idx == state.tools_menu_index;
            let marker = if selected { ">" } else { " " };
            let status = match tool.approved {
                Some(true) => "approved",
                Some(false) => "denied",
                None => "pending",
            };
            lines.push(Line::styled(
                format!(
                    "{marker} [{}] {} - {}",
                    status, tool.tool_id, tool.description
                ),
                if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                },
            ));
            if selected {
                lines.push(Line::styled(
                    format!("    args: {}", tool.args),
                    Style::default().fg(Color::DarkGray),
                ));
                lines.push(Line::styled(
                    format!("    risk: {}", tool.risk_level),
                    Style::default().fg(Color::DarkGray),
                ));
                for reason in &tool.reasons {
                    lines.push(Line::styled(
                        format!("    why: {reason}"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        }
    }

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/tools").borders(Borders::ALL))
        .render(popup_area, buf);
}

fn render_help_menu(area: Rect, buf: &mut Buffer) {
    let popup_area = centered_rect(area.width.saturating_mul(3) / 5, area.height / 2, area);

    let lines = vec![
        Line::styled(
            "Slash Commands",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw("/help      Open this help menu"),
        Line::raw("/model     Open model selector"),
        Line::raw("/settings  Open settings editor"),
        Line::raw("/sessions  Open sessions menu"),
        Line::raw("/tools     Open tools approval menu"),
        Line::raw("/new       Start a new session"),
        Line::raw("/quit      Exit the TUI"),
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
