use crate::components::normalize_terminal_text;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use super::super::{AppState, ScriptApprovalAction};

pub(super) fn render_script_approval_panel(state: &AppState, area: Rect, buf: &mut Buffer) {
    let Some(script) = state.pending_script.as_ref() else {
        return;
    };

    let block = Block::default()
        .title(" Permission required ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    Clear.render(area, buf);
    block.render(area, buf);
    for y in area.y..area.y + area.height {
        buf[(area.x, y)].set_char(' ').set_bg(Color::Yellow);
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
            "Run Nushell script with additional capabilities",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            normalize_terminal_text(&format!(
                "additional: {}  timeout: {} ms",
                script.capability_additions.join(", "),
                script.timeout_millis
            ))
            .into_owned(),
            Style::default().fg(Color::Gray),
        ),
    ];
    lines.push(Line::styled(
        normalize_terminal_text(&format!(
            "requested: {}",
            script.requested_capabilities.join(", ")
        ))
        .into_owned(),
        Style::default().fg(Color::Gray),
    ));
    lines.push(Line::raw(String::new()));
    lines.push(Line::styled("script", Style::default().fg(Color::Gray)));
    lines.push(Line::raw(
        normalize_terminal_text(&script.source).into_owned(),
    ));
    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .render(body_area, buf);

    let allow_style = if state.script_approval_action == ScriptApprovalAction::Allow {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };
    let reject_style = if state.script_approval_action == ScriptApprovalAction::Reject {
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
