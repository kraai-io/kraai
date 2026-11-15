use std::io::Write;

use base64::Engine;
use color_eyre::eyre::Result;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    widgets::Widget,
};

use crate::components::ChatHistory;

use super::{AppState, UiMode};

mod error;
mod script_approval;
mod sheet;
mod sheets;
mod status;
use script_approval::render_script_approval_panel;
use status::render_status;
pub(super) use status::{activity_text, format_token_count};

pub(super) fn bottom_panel_height(state: &AppState, area: Rect) -> u16 {
    let available = area.height.saturating_sub(footer_height(state, area));
    if state.mode == UiMode::Chat && state.has_local_script_approval() {
        if state.approval_expanded {
            available
        } else {
            12.min(available)
        }
    } else {
        state
            .composer()
            .get_height(area.width)
            .min(10)
            .min((area.height / 3).max(3))
            .min(available)
    }
}

fn footer_height(state: &AppState, area: Rect) -> u16 {
    (2 + u16::from(state.last_error.is_some())).min(area.height.saturating_sub(1))
}

pub(super) fn chat_layout(state: &AppState, area: Rect) -> [Rect; 3] {
    Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(footer_height(state, area)),
        Constraint::Length(bottom_panel_height(state, area)),
    ])
    .flex(Flex::End)
    .areas(area)
}

impl Widget for &AppState {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let [chat_history_area, status_area, input_area] = chat_layout(self, area);

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
        render_status(self, status_area, buf);

        if self.mode == UiMode::Chat && self.has_local_script_approval() {
            render_script_approval_panel(self, input_area, buf);
        } else {
            self.composer().render(input_area, buf);
        }
        if let Some(sheet) = sheets::active(self) {
            sheet::render(&sheet, &self.sheet_scroll, chat_history_area, area, buf);
        }

        error::render_error(self, area, buf);
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

pub(super) fn next_wrapping(current_index: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }

    (current_index + 1) % len
}

pub(super) fn previous_wrapping(current_index: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }

    (current_index + len - 1) % len
}
