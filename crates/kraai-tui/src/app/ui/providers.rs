use std::collections::HashMap;

use kraai_runtime::{FieldDefinition, ModelSettings, ProviderSettings};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use super::super::{
    ActiveSettingsEditor, AppState, ProviderAuthState, ProvidersAdvancedFocus, ProvidersView,
    SettingsModelField, SettingsProviderField, field_value_display, provider_definition_rank,
};
use super::{centered_rect, menu_scroll_offset, selection_style};

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

pub(in crate::app) fn parse_settings_errors(message: &str) -> HashMap<String, String> {
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

pub(super) fn render_providers_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
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
                let marker = if selected { "⮞" } else { " " };
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
            let marker = if selected { "⮞" } else { " " };
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
                        if selected { "⮞" } else { " " },
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
                                if selected { "⮞" } else { " " },
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
                            if selected { "⮞" } else { " " },
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
