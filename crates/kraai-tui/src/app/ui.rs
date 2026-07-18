use std::io::Write;

use base64::Engine;
use color_eyre::eyre::Result;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Style},
    widgets::{Paragraph, Widget},
};

use crate::components::{ChatHistory, TextInput};

use super::{AppState, ScriptPhase, UiMode};

mod command_popup;
mod menus;
mod providers;
mod script_approval;
mod status;
use command_popup::render_command_popup;
use menus::{render_agent_menu, render_help_menu, render_model_menu, render_sessions_menu};
pub(super) use providers::parse_settings_errors;
use providers::render_providers_menu;
use script_approval::render_script_approval_panel;
pub(super) use status::format_token_count;
use status::statusline_line;

pub(super) const STATUSLINE_STREAMING_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub(super) fn bottom_panel_height(state: &AppState, area: Rect) -> u16 {
    if state.mode == UiMode::Chat && state.script_phase == ScriptPhase::AwaitingApproval {
        10.min(area.height.saturating_sub(1).max(3))
    } else {
        TextInput::new(&state.input, state.input_cursor).get_height(area.width)
    }
}

impl Widget for &AppState {
    fn render(self, area: Rect, buf: &mut Buffer)
    where
        Self: Sized,
    {
        let input_height = bottom_panel_height(self, area);
        let layout = Layout::vertical([
            Constraint::Min(area.height.saturating_sub(input_height + 1)),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .flex(Flex::End);
        let [chat_history_area, status_area, input_area] = layout.areas(area);

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
        Paragraph::new(statusline_line(self))
            .style(Style::default().fg(Color::DarkGray))
            .render(status_area, buf);

        if self.mode == UiMode::Chat && self.script_phase == ScriptPhase::AwaitingApproval {
            render_script_approval_panel(self, input_area, buf);
        } else {
            TextInput::new(&self.input, self.input_cursor).render(input_area, buf);
        }
        if self.mode == UiMode::Chat && self.script_phase != ScriptPhase::AwaitingApproval {
            render_command_popup(self, area, input_area, buf);
        }

        match self.mode {
            UiMode::AgentMenu => render_agent_menu(self, area, buf),
            UiMode::ModelMenu => render_model_menu(self, area, buf),
            UiMode::ProvidersMenu => render_providers_menu(self, area, buf),
            UiMode::SessionsMenu => render_sessions_menu(self, area, buf),
            UiMode::Help => render_help_menu(area, buf),
            UiMode::Chat => {}
        }
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

pub(super) fn active_command_prefix(input: &str) -> Option<&str> {
    let cmd = input.strip_prefix('/')?;
    if cmd.chars().any(char::is_whitespace) {
        return None;
    }
    Some(cmd)
}

pub(super) fn is_known_slash_command(command_line: &str) -> bool {
    command_line
        .split_whitespace()
        .next()
        .is_some_and(|command| {
            super::SLASH_COMMANDS
                .iter()
                .any(|(known, _)| *known == command)
        })
}

pub(super) fn slash_command_matches(prefix: &str) -> Vec<(&'static str, &'static str)> {
    super::SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|(command, _)| command.starts_with(prefix))
        .collect()
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
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}
