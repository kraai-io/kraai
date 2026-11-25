use super::types::UiMode;
use std::time::{Duration, Instant};

use ratatui::{Terminal, backend::Backend, layout::Position};

use super::AppState;

pub(super) struct CursorBlink {
    epoch: Instant,
    pub(super) visible: bool,
    pub(super) position: Option<Position>,
}

impl CursorBlink {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            epoch: now,
            visible: true,
            position: None,
        }
    }

    pub(super) fn show<B: Backend>(&self, terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        let Some(position) = self.position else {
            return Ok(());
        };
        if self.visible {
            terminal.set_cursor_position(position)?;
            terminal.show_cursor()
        } else {
            terminal.hide_cursor()
        }
    }

    pub(super) fn update(&mut self, now: Instant, input: bool) -> bool {
        if input {
            self.epoch = now;
        }
        let visible = (now.duration_since(self.epoch).as_millis() / 500).is_multiple_of(2);
        let changed = visible != self.visible;
        self.visible = visible;
        changed
    }

    pub(super) fn timeout(&self, now: Instant) -> Duration {
        Duration::from_millis(500 - (now.duration_since(self.epoch).as_millis() % 500) as u64)
    }
}

impl AppState {
    pub(super) fn composer_cursor_enabled(&self) -> bool {
        !(self.errors.open && self.errors.last.is_some())
            && match self.mode {
                UiMode::Chat => !self.has_local_script_approval(),
                UiMode::Agent
                | UiMode::Model
                | UiMode::ModelSettings
                | UiMode::Providers
                | UiMode::Mcp
                | UiMode::Sessions => true,
            }
    }
}

#[cfg(test)]
mod tests;
