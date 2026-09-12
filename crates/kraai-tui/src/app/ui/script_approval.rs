use crate::components::{ChatHistory, normalize_terminal_text};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
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
    let inner = block.inner(area);
    Clear.render(area, buf);
    block.render(area, buf);
    let metadata = ChatHistory::wrap_with_prefix(
        &normalize_terminal_text(&format!(
            "Additional: {}\nRequested: {}\nTimeout: {} ms",
            script.capability_additions.join(", "),
            script.requested_capabilities.join(", "),
            script.timeout_millis
        )),
        inner.width as usize,
        "",
        "",
    );
    let metadata_height = metadata.len().min(u16::MAX as usize) as u16;
    let [metadata_area, source_area, footer_area] = Layout::vertical([
        Constraint::Length(metadata_height.min(inner.height.saturating_sub(2))),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    Paragraph::new(metadata.into_iter().map(Line::raw).collect::<Vec<_>>())
        .render(metadata_area, buf);
    let source = ChatHistory::wrap_with_prefix(
        &normalize_terminal_text(&script.source),
        source_area.width as usize,
        "",
        "",
    );
    let max_scroll = source.len().saturating_sub(source_area.height as usize);
    state
        .approval_scroll
        .set(state.approval_scroll.get().min(max_scroll));
    Paragraph::new(
        source
            .into_iter()
            .skip(state.approval_scroll.get())
            .take(source_area.height as usize)
            .map(Line::raw)
            .collect::<Vec<_>>(),
    )
    .render(source_area, buf);
    let action_style = |action| {
        if state.script_approval_action == action {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default().fg(Color::Gray)
        }
    };
    Paragraph::new(Line::from(vec![
        Span::styled("Allow", action_style(ScriptApprovalAction::Allow)),
        Span::raw(" / "),
        Span::styled("Reject", action_style(ScriptApprovalAction::Reject)),
        Span::raw("  ←/→ Enter  ↑/↓ scroll  f expand  Home/End"),
    ]))
    .render(footer_area, buf);
}
