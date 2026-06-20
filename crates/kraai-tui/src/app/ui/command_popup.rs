use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::super::AppState;
use super::{active_command_prefix, menu_scroll_offset, slash_command_matches};

pub(super) fn render_command_popup(
    state: &AppState,
    area: Rect,
    input_area: Rect,
    buf: &mut Buffer,
) {
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
