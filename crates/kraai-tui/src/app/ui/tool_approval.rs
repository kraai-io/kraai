use crate::components::normalize_terminal_text;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use super::super::{AppState, ToolApprovalAction};

pub(super) fn render_tool_approval_panel(state: &AppState, area: Rect, buf: &mut Buffer) {
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
            normalize_terminal_text(&tool.description).into_owned(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            normalize_terminal_text(&format!(
                "tool: {}  risk: {}",
                tool.tool_id, tool.risk_level
            ))
            .into_owned(),
            Style::default().fg(Color::Gray),
        ),
    ];
    for reason in &tool.reasons {
        lines.push(Line::styled(
            normalize_terminal_text(&format!("why: {reason}")).into_owned(),
            Style::default().fg(Color::Gray),
        ));
    }
    lines.push(Line::raw(String::new()));
    lines.push(Line::styled("args", Style::default().fg(Color::Gray)));
    lines.push(Line::raw(normalize_terminal_text(&tool.args).into_owned()));
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
