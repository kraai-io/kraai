use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::components::{ChatHistory, normalize_terminal_text};

use super::AppState;

pub(super) fn render_error(state: &AppState, area: Rect, buf: &mut Buffer) {
    if !state.error_open {
        return;
    }
    let Some(error) = &state.last_error else {
        return;
    };
    let popup = super::centered_rect(
        area.width.saturating_sub(4).max(area.width.min(20)),
        area.height.saturating_sub(2).max(area.height.min(5)),
        area,
    );
    Clear.render(popup, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Error ")
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(popup);
    block.render(popup, buf);
    let show_status = !state.status.is_empty() && state.status != *error && inner.height > 1;
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(if show_status { 2 } else { 1 }),
    );
    let lines = ChatHistory::wrap_with_prefix(
        &normalize_terminal_text(error),
        usize::from(body.width),
        "",
        "",
    );
    let offset = state
        .error_scroll
        .get()
        .min(lines.len().saturating_sub(usize::from(body.height)));
    state.error_scroll.set(offset);
    let visible: Vec<Line<'_>> = lines
        .iter()
        .skip(offset)
        .take(usize::from(body.height))
        .map(|line| Line::raw(line.as_str()))
        .collect();
    Paragraph::new(visible).render(body, buf);
    if show_status {
        Paragraph::new(normalize_terminal_text(&state.status).replace('\n', " "))
            .style(Style::default().fg(Color::Gray))
            .render(Rect::new(inner.x, inner.bottom() - 2, inner.width, 1), buf);
    }
    if inner.height > 0 {
        let copy = if state
            .feedback
            .copied(super::super::feedback::CopyTarget::Error)
        {
            "Copied"
        } else {
            "c copy"
        };
        Paragraph::new(format!("↑/↓ scroll · {copy} · d dismiss · Esc close"))
            .style(Style::default().fg(Color::DarkGray))
            .render(Rect::new(inner.x, inner.bottom() - 1, inner.width, 1), buf);
    }
}
