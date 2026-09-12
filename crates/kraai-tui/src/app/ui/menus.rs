use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::super::AppState;
use super::{centered_rect, menu_scroll_offset};

pub(super) fn render_model_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let models = state.filtered_models();
    let popup_area = centered_rect(area.width.saturating_mul(3) / 4, area.height / 2, area);

    let mut lines = vec![Line::styled(
        "Select model (Enter to choose, Esc to close)",
        Style::default().add_modifier(Modifier::BOLD),
    )];

    if models.is_empty() {
        lines.push(Line::raw("No matching models"));
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
        .block(
            Block::default()
                .title(format!("/model  Filter: {}", state.menu_search))
                .borders(Borders::ALL),
        )
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
            let capabilities = profile
                .capabilities
                .iter()
                .map(kraai_types::SandboxCapability::as_str)
                .collect::<Vec<_>>()
                .join(",");
            lines.push(Line::raw(format!(
                "  {} | capabilities={} | escalation={} | source={}",
                profile.description,
                capabilities,
                profile.escalation_policy.as_str(),
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
    let sessions = state.filtered_sessions();
    let total_lines = sessions.len() * 2 + 2;
    let selected_line = state
        .sessions_menu_index
        .saturating_mul(2)
        .saturating_add(1);
    let scroll_offset = menu_scroll_offset(selected_line, total_lines, visible_lines);

    let mut lines = vec![Line::styled(
        "Sessions (Enter=load/new, Delete=delete, Esc=close)",
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

    for (idx, session) in sessions.iter().enumerate() {
        let selected = state.sessions_menu_index == idx + 1;
        let marker = if selected { "⮞" } else { " " };
        let current = state
            .current_session_id
            .as_ref()
            .is_some_and(|sid| sid == &session.id);
        let title = session.title.clone().unwrap_or_else(|| {
            format!("Session {}", session.id.chars().take(8).collect::<String>())
        });
        let current_suffix = if current { " (current)" } else { "" };
        let approval_suffix = if session.waiting_for_approval {
            " [approval]"
        } else {
            ""
        };
        let running_suffix = if session.is_running { " [running]" } else { "" };
        lines.push(Line::styled(
            format!("{marker} {title}{current_suffix}{approval_suffix}{running_suffix}"),
            if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            },
        ));
        let age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(session.updated_at);
        let age = if age < 60 {
            format!("{age}s ago")
        } else if age < 3600 {
            format!("{}m ago", age / 60)
        } else if age < 86400 {
            format!("{}h ago", age / 3600)
        } else {
            format!("{}d ago", age / 86400)
        };
        lines.push(Line::styled(
            format!("    {age} · {}", session.workspace_dir),
            Style::default().fg(Color::DarkGray),
        ));
    }

    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title(format!("/sessions  Filter: {}", state.menu_search))
                .borders(Borders::ALL),
        )
        .scroll((scroll_offset as u16, 0))
        .render(popup_area, buf);
}

pub(super) fn render_help_menu(state: &AppState, area: Rect, buf: &mut Buffer) {
    let lines = vec![
        Line::raw("/            Commands"),
        Line::raw("Enter        Send       Shift+Enter  Newline"),
        Line::raw("↑/↓          History    Ctrl+E       Editor"),
        Line::raw("Ctrl+←/→     Move word  Ctrl+W       Delete word"),
        Line::raw("PgUp/PgDn    Scroll     Home/End     First/last"),
        Line::raw("F6           Executions"),
        Line::raw("Esc          Close / cancel"),
    ];
    let popup_area = centered_rect(
        area.width.min(52),
        area.height.min(lines.len() as u16 + 2),
        area,
    );

    let max_scroll =
        (lines.len() as u16).saturating_sub(popup_area.height.saturating_sub(2).max(1));
    let scroll = state.help_scroll.get().min(max_scroll);
    state.help_scroll.set(scroll);
    Clear.render(popup_area, buf);
    Paragraph::new(Text::from(lines))
        .scroll((scroll, 0))
        .block(
            Block::default()
                .title("Help · ↑/↓ scroll · Esc close")
                .borders(Borders::ALL),
        )
        .render(popup_area, buf);
}
