use std::io::Write;

use base64::Engine;
use color_eyre::eyre::Result;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Widget},
};

use crate::components::ChatHistory;

use super::{AppState, UiMode};

mod error;
mod mcp;
mod providers;
mod script_approval;
mod sheet;
mod sheets;
mod status;
use providers::render_providers_menu;
use script_approval::render_script_approval_panel;
pub(super) use status::format_token_count;
use status::render_status;

fn menu_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
}

pub(super) const STATUSLINE_STREAMING_FRAMES: [&str; 8] =
    ["·  ", "·· ", "···", " ··", "  ·", " ··", "···", "·· "];

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

        match self.mode {
            UiMode::ProvidersMenu => render_providers_menu(self, area, buf),
            UiMode::Mcp => mcp::render_mcp(self, area, buf),
            UiMode::Chat
            | UiMode::Agent
            | UiMode::Model
            | UiMode::ModelSettings
            | UiMode::Sessions => {}
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

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let popup = Layout::vertical([
        Constraint::Length((area.height.saturating_sub(height)) / 2),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .split(area)
    .get(1)
    .copied()
    .unwrap_or(area);

    Layout::horizontal([
        Constraint::Length((area.width.saturating_sub(width)) / 2),
        Constraint::Length(width),
        Constraint::Min(0),
    ])
    .split(popup)
    .get(1)
    .copied()
    .unwrap_or(popup)
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

fn selection_style(active: bool) -> Style {
    if active {
        Style::default().fg(crate::theme::ACCENT)
    } else {
        Style::default()
    }
}
