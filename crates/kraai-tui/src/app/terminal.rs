use super::App;
use super::commands::active_command_prefix;
use super::types::{ProvidersView, RuntimeRequest, ScriptPhase, UiMode};
use super::{palette, terminal_reply};
use color_eyre::eyre::Result;
use ratatui::crossterm::event::{
    Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use std::time::{Duration, Instant};

const WHEEL_SCROLL_LINES: i16 = 3;
const TERMINAL_EVENT_BATCH_BUDGET: Duration = Duration::from_millis(1);

impl App {
    pub(super) fn handle_events(&mut self, timeout: Option<Duration>) -> Result<bool> {
        self.wait_for_events(timeout);
        if self.hangup.try_recv().is_ok() {
            self.state.exit = true;
        }
        let mut changed = false;
        let batch_started = Instant::now();
        while !self.state.exit && batch_started.elapsed() < TERMINAL_EVENT_BATCH_BUDGET {
            let Ok(event) = self.input.receiver().try_recv() else {
                break;
            };
            changed |= self.handle_terminal_event(event?);
        }
        Ok(changed)
    }

    fn wait_for_events(&self, timeout: Option<Duration>) {
        let mut select = crossbeam_channel::Select::new();
        select.recv(self.input.receiver());
        select.recv(&self.event_rx);
        select.recv(&self.runtime_rx);
        select.recv(&self.hangup);
        match timeout {
            Some(timeout) => {
                let _ = select.ready_timeout(timeout);
            }
            None => {
                select.ready();
            }
        }
    }

    pub(super) fn handle_terminal_event(&mut self, event: CrosstermEvent) -> bool {
        match self.state.cursor_preferences.filter(event) {
            terminal_reply::FilteredEvent::Pass(event) => self.handle_palette_event(event),
            terminal_reply::FilteredEvent::Consumed => false,
            terminal_reply::FilteredEvent::Replay(events) => {
                let mut changed = false;
                for event in events {
                    changed |= self.handle_palette_event(event);
                }
                changed
            }
        }
    }

    fn handle_palette_event(&mut self, event: CrosstermEvent) -> bool {
        match self.state.palette.filter(event) {
            palette::PaletteEvent::Pass(event) => self.handle_input_event(event),
            palette::PaletteEvent::Consumed => false,
            palette::PaletteEvent::Replay(events) => {
                let mut changed = false;
                for event in events {
                    changed |= self.handle_input_event(event);
                }
                changed
            }
        }
    }

    fn handle_input_event(&mut self, event: CrosstermEvent) -> bool {
        if self.state.exit {
            return false;
        }
        match event {
            CrosstermEvent::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                self.handle_key_event(key_event);
                true
            }
            CrosstermEvent::Mouse(mouse_event) => {
                self.handle_mouse_event(mouse_event);
                true
            }
            CrosstermEvent::Paste(text) => {
                self.handle_paste(text);
                true
            }
            CrosstermEvent::Resize(_, _) => true,
            CrosstermEvent::FocusGained | CrosstermEvent::FocusLost | CrosstermEvent::Key(_) => {
                false
            }
        }
    }

    pub(super) fn handle_mouse_event(&mut self, mouse_event: MouseEvent) {
        if self.state.errors.open && self.state.errors.last.is_some() {
            let offset = self.state.errors.scroll.get();
            match mouse_event.kind {
                MouseEventKind::ScrollUp => self.state.errors.scroll.set(offset.saturating_sub(3)),
                MouseEventKind::ScrollDown => {
                    self.state.errors.scroll.set(offset.saturating_add(3))
                }
                MouseEventKind::Down(_)
                | MouseEventKind::Up(_)
                | MouseEventKind::Drag(_)
                | MouseEventKind::Moved
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight => {}
            }
            return;
        }
        if self.state.mode != UiMode::Chat {
            return;
        }

        if self.state.mode == UiMode::Chat && self.state.has_local_script_approval() {
            match mouse_event.kind {
                MouseEventKind::ScrollUp => self
                    .state
                    .turn
                    .approval_scroll
                    .set(self.state.turn.approval_scroll.get().saturating_sub(3)),
                MouseEventKind::ScrollDown => self
                    .state
                    .turn
                    .approval_scroll
                    .set(self.state.turn.approval_scroll.get().saturating_add(3)),
                MouseEventKind::Down(_)
                | MouseEventKind::Up(_)
                | MouseEventKind::Drag(_)
                | MouseEventKind::Moved
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight => {}
            }
            return;
        }
        match mouse_event.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_chat_by(-WHEEL_SCROLL_LINES);
            }
            MouseEventKind::ScrollDown => {
                self.scroll_chat_by(WHEEL_SCROLL_LINES);
            }
            MouseEventKind::Down(_)
            | MouseEventKind::Up(_)
            | MouseEventKind::Drag(_)
            | MouseEventKind::Moved
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {}
        }
    }

    pub(super) fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.handle_error_key(key_event) {
            return;
        }
        if self.state.mode == UiMode::Chat && self.state.has_local_script_approval() {
            if matches!(key_event.code, KeyCode::Esc) {
                return;
            }
            self.handle_script_approval_key_event(key_event);
            return;
        }

        if matches!(key_event.code, KeyCode::Esc) {
            if self.state.mode == UiMode::Chat && self.command_popup_visible() {
                self.state.composer.command_popup_dismissed = true;
                self.reset_command_selection();
                return;
            }
            if self.state.mode == UiMode::Chat
                && (self.state.turn.is_streaming
                    || self.state.turn.retry_waiting
                    || self.state.turn.script_phase == ScriptPhase::Executing)
            {
                if let Some(session_id) = &self.state.current_session_id {
                    self.request(RuntimeRequest::CancelStream {
                        session_id: session_id.clone(),
                    });
                }
                return;
            }
            match self.state.mode {
                UiMode::Providers => self.close_providers_level(),
                UiMode::ModelSettings => self.close_model_settings(),
                UiMode::Mcp => self.close_mcp_level(),
                UiMode::Model | UiMode::Chat | UiMode::Agent | UiMode::Sessions => {
                    self.close_sheet()
                }
            }
            return;
        }

        match self.state.mode {
            UiMode::Chat => self.handle_chat_key_event(key_event),
            UiMode::Agent => self.handle_agent_key_event(key_event),
            UiMode::Model => self.handle_model_key_event(key_event),
            UiMode::ModelSettings => self.handle_model_settings_key_event(key_event),
            UiMode::Providers => self.handle_providers_key_event(key_event),
            UiMode::Mcp => self.handle_mcp_key_event(key_event),
            UiMode::Sessions => self.handle_sessions_key_event(key_event),
        }
    }

    pub(super) fn handle_chat_key_event(&mut self, key_event: KeyEvent) {
        if is_ctrl_c(key_event) {
            self.handle_ctrl_c();
            return;
        }

        self.state.ctrl_c_exit_armed = false;
        if self.handle_composer_shortcut(key_event) {
            return;
        }
        if key_event.code == KeyCode::Char('o') && key_event.modifiers == KeyModifiers::CONTROL {
            self.state.chat.scripts_expanded = !self.state.chat.scripts_expanded;
            self.state.chat.invalidate();
            return;
        }
        if self.handle_command_list_key(key_event) {
            return;
        }

        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "every other key does nothing in the composer"
        )]
        match key_event.code {
            KeyCode::Enter => {
                self.reset_command_selection();
                if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                    self.insert_input_char('\n');
                } else {
                    self.handle_submit();
                }
            }
            KeyCode::Char(c)
                if !key_event
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_input_char(c);
                if active_command_prefix(&self.state.composer.text).is_none() {
                    self.state.composer.command_popup_dismissed = false;
                }
                self.reset_command_selection();
            }
            KeyCode::Delete => self.delete_input_char(),
            KeyCode::Backspace => {
                self.backspace_input_char();
                if active_command_prefix(&self.state.composer.text).is_none() {
                    self.state.composer.command_popup_dismissed = false;
                }
                self.reset_command_selection();
            }
            KeyCode::Up => self.handle_input_up(),
            KeyCode::Down => self.handle_input_down(),
            KeyCode::Left => {
                self.move_input_cursor_left();
            }
            KeyCode::Right => {
                self.move_input_cursor_right();
            }
            KeyCode::PageUp => {
                self.page_chat(false);
            }
            KeyCode::PageDown => {
                self.page_chat(true);
            }
            KeyCode::Home => {
                self.scroll_chat_to_top();
            }
            KeyCode::End => {
                self.scroll_chat_to_bottom();
            }
            _ => {}
        }
    }

    pub(super) fn handle_paste(&mut self, text: String) {
        if self.state.errors.open && self.state.errors.last.is_some() {
            return;
        }
        self.state.sheet.armed = false;
        match self.state.mode {
            UiMode::Agent | UiMode::Mcp | UiMode::Model | UiMode::Sessions => {
                self.paste_into_sheet_query(&text);
                self.state.sheet.reset_selection();
                return;
            }
            UiMode::ModelSettings => {
                self.paste_into_sheet_query(&text);
                return;
            }
            UiMode::Providers if self.state.providers.view == ProvidersView::Config => {
                self.paste_into_provider_config(&text);
                return;
            }
            UiMode::Providers => {
                self.paste_into_sheet_query(&text);
                self.state.sheet.reset_selection();
                return;
            }
            UiMode::Chat => {}
        }
        if self.state.has_local_script_approval() || text.is_empty() {
            return;
        }

        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.insert_input_text(&text);
        if active_command_prefix(&self.state.composer.text).is_none() {
            self.state.composer.command_popup_dismissed = false;
        }
        self.reset_command_selection();
    }

    pub(super) fn handle_ctrl_c(&mut self) {
        if self.state.ctrl_c_exit_armed {
            self.state.exit = true;
            return;
        }

        self.clear_chat_transient_state();
        self.state.ctrl_c_exit_armed = true;
    }

    pub(super) fn clear_chat_transient_state(&mut self) {
        self.clear_message_draft();
        self.state.composer.command_popup_dismissed = false;
        self.reset_command_selection();
    }

    pub(super) fn handle_script_approval_key_event(&mut self, key_event: KeyEvent) {
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "every other key does nothing while a script approval is open"
        )]
        match key_event.code {
            KeyCode::Left | KeyCode::BackTab => self.select_previous_script_action(),
            KeyCode::Right | KeyCode::Tab => self.select_next_script_action(),
            KeyCode::Enter => self.confirm_current_script_action(),
            KeyCode::Up => self
                .state
                .turn
                .approval_scroll
                .set(self.state.turn.approval_scroll.get().saturating_sub(1)),
            KeyCode::Down => self
                .state
                .turn
                .approval_scroll
                .set(self.state.turn.approval_scroll.get().saturating_add(1)),
            KeyCode::PageUp => self
                .state
                .turn
                .approval_scroll
                .set(self.state.turn.approval_scroll.get().saturating_sub(10)),
            KeyCode::PageDown => self
                .state
                .turn
                .approval_scroll
                .set(self.state.turn.approval_scroll.get().saturating_add(10)),
            KeyCode::Home => self.state.turn.approval_scroll.set(0),
            KeyCode::End => self.state.turn.approval_scroll.set(usize::MAX),
            KeyCode::Char('f') => {
                self.state.turn.approval_expanded = !self.state.turn.approval_expanded
            }
            _ => {}
        }
    }
}

fn is_ctrl_c(key_event: KeyEvent) -> bool {
    key_event.code == KeyCode::Char('c') && key_event.modifiers == KeyModifiers::CONTROL
}
