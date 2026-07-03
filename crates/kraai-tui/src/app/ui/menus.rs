use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::super::{AppState, flatten_models_map};
use super::{centered_rect, menu_scroll_offset};

pub(super) fn render_model_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
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
            let marker = if selected { "⮞" } else { " " };
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

pub(super) fn render_agent_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
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
            let marker = if selected { "⮞" } else { " " };
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

pub(super) fn render_sessions_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
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
        let marker = if selected { "⮞" } else { " " };
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

pub(super) fn render_help_menu(area: Rect, buf: &mut Buffer) {
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
        Line::raw(""),
        Line::raw("Esc closes menus."),
    ];

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(Block::default().title("/help").borders(Borders::ALL))
        .render(popup_area, buf);
}
