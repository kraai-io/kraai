use super::*;

impl App {
    pub(super) fn handle_events(&mut self, timeout: std::time::Duration) -> Result<bool> {
        if !event::poll(timeout)? {
            return Ok(false);
        }

        let mut changed = false;
        loop {
            if self.handle_terminal_event(event::read()?) {
                changed = true;
            }

            if !event::poll(std::time::Duration::from_millis(0))? {
                break;
            }
        }

        Ok(changed)
    }

    pub(super) fn handle_terminal_event(&mut self, event: CrosstermEvent) -> bool {
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
            CrosstermEvent::Resize(_, _) => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                true
            }
            _ => false,
        }
    }

    pub(super) fn handle_mouse_event(&mut self, mouse_event: MouseEvent) {
        if self.state.mode != UiMode::Chat || self.state.tool_phase == ToolPhase::Deciding {
            return;
        }

        match mouse_event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(position) = self.hit_test_chat_cell(mouse_event.column, mouse_event.row)
                {
                    self.state.selection = Some(ChatSelection {
                        anchor: position,
                        focus: position,
                    });
                } else {
                    self.clear_chat_selection();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(position) = self.hit_test_chat_cell(mouse_event.column, mouse_event.row)
                    && let Some(selection) = self.state.selection.as_mut()
                {
                    selection.focus = position;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(position) = self.hit_test_chat_cell(mouse_event.column, mouse_event.row)
                    && let Some(selection) = self.state.selection.as_mut()
                {
                    selection.focus = position;
                }
            }
            MouseEventKind::ScrollUp => {
                self.scroll_chat_by(-1);
            }
            MouseEventKind::ScrollDown => {
                self.scroll_chat_by(1);
            }
            _ => {}
        }
    }

    pub(super) fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.state.mode == UiMode::Chat && self.state.tool_phase == ToolPhase::Deciding {
            if matches!(key_event.code, KeyCode::Esc) {
                return;
            }
            self.handle_tool_approval_key_event(key_event);
            return;
        }

        if matches!(key_event.code, KeyCode::Esc) {
            if self.state.mode == UiMode::Chat && self.command_popup_visible() {
                self.state.command_popup_dismissed = true;
                self.reset_completion_cycle();
                return;
            }
            if self.state.mode == UiMode::Chat
                && (self.state.is_streaming || self.state.retry_waiting)
            {
                if let Some(session_id) = &self.state.current_session_id {
                    self.request(RuntimeRequest::CancelStream {
                        session_id: session_id.clone(),
                    });
                }
                return;
            }
            if self.state.mode == UiMode::ProvidersMenu {
                self.handle_providers_escape();
                return;
            }
            self.clear_chat_selection();
            self.state.visible_chat_view = None;
            self.state.mode = UiMode::Chat;
            return;
        }

        match self.state.mode {
            UiMode::Chat => self.handle_chat_key_event(key_event),
            UiMode::AgentMenu => self.handle_agent_menu_key_event(key_event),
            UiMode::ModelMenu => self.handle_model_menu_key_event(key_event),
            UiMode::ProvidersMenu => self.handle_providers_key_event(key_event),
            UiMode::SessionsMenu => self.handle_sessions_menu_key_event(key_event),
            UiMode::Help => {
                if matches!(key_event.code, KeyCode::Enter | KeyCode::Char('q')) {
                    self.clear_chat_selection();
                    self.state.mode = UiMode::Chat;
                }
            }
        }
    }

    pub(super) fn handle_chat_key_event(&mut self, key_event: KeyEvent) {
        if is_ctrl_c(key_event) {
            self.handle_ctrl_c();
            return;
        }

        self.state.ctrl_c_exit_armed = false;

        if is_copy_shortcut(key_event) {
            self.copy_selection_to_clipboard();
            return;
        }

        match key_event.code {
            KeyCode::Enter => {
                if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                    self.insert_input_char('\n');
                    self.reset_completion_cycle();
                } else if !self.execute_current_command_suggestion() {
                    self.reset_completion_cycle();
                    self.handle_submit();
                }
            }
            KeyCode::Tab => {
                self.cycle_command_suggestion(true);
            }
            KeyCode::BackTab => {
                self.cycle_command_suggestion(false);
            }
            KeyCode::Char(c) => {
                self.insert_input_char(c);
                if active_command_prefix(&self.state.input).is_none() {
                    self.state.command_popup_dismissed = false;
                }
                self.reset_completion_cycle();
            }
            KeyCode::Backspace => {
                self.backspace_input_char();
                if active_command_prefix(&self.state.input).is_none() {
                    self.state.command_popup_dismissed = false;
                }
                self.reset_completion_cycle();
            }
            KeyCode::Up => {
                if active_command_prefix(&self.state.input).is_some() {
                    self.cycle_command_suggestion(false);
                } else {
                    self.state.input_cursor = 0;
                }
            }
            KeyCode::Down => {
                if active_command_prefix(&self.state.input).is_some() {
                    self.cycle_command_suggestion(true);
                } else {
                    self.state.input_cursor = self.state.input.len();
                }
            }
            KeyCode::Left => {
                self.move_input_cursor_left();
            }
            KeyCode::Right => {
                self.move_input_cursor_right();
            }
            KeyCode::PageUp => {
                self.scroll_chat_by(-10);
            }
            KeyCode::PageDown => {
                self.scroll_chat_by(10);
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
        if self.state.mode != UiMode::Chat
            || self.state.tool_phase == ToolPhase::Deciding
            || text.is_empty()
        {
            return;
        }

        self.insert_input_text(&text);
        if active_command_prefix(&self.state.input).is_none() {
            self.state.command_popup_dismissed = false;
        }
        self.reset_completion_cycle();
    }

    pub(super) fn handle_ctrl_c(&mut self) {
        if self.state.ctrl_c_exit_armed {
            self.state.exit = true;
            return;
        }

        self.clear_chat_transient_state();
        self.state.ctrl_c_exit_armed = true;
        self.state.status = String::from("Cleared input. Press Ctrl+C again to exit");
    }

    pub(super) fn clear_chat_transient_state(&mut self) {
        self.state.input.clear();
        self.state.input_cursor = 0;
        self.clear_chat_selection();
        self.state.visible_chat_view = None;
        self.state.command_popup_dismissed = false;
        self.reset_completion_cycle();
    }

    pub(super) fn reset_completion_cycle(&mut self) {
        self.state.command_completion_prefix = None;
        self.state.command_completion_index = 0;
    }

    pub(super) fn cycle_command_suggestion(&mut self, forward: bool) {
        if self.state.command_popup_dismissed {
            return;
        }
        let Some(prefix) = active_command_prefix(&self.state.input) else {
            return;
        };
        let matches = slash_command_matches(prefix);
        if matches.is_empty() {
            self.state.status = format!("No command matches '/{prefix}'");
            return;
        }

        let next_index = if self.state.command_completion_prefix.as_deref() == Some(prefix) {
            if forward {
                (self.state.command_completion_index + 1) % matches.len()
            } else if self.state.command_completion_index == 0 {
                matches.len() - 1
            } else {
                self.state.command_completion_index - 1
            }
        } else if forward {
            usize::from(matches.len() > 1)
        } else {
            matches.len() - 1
        };

        self.state.command_completion_prefix = Some(prefix.to_string());
        self.state.command_completion_index = next_index;
    }

    pub(super) fn execute_current_command_suggestion(&mut self) -> bool {
        if self.state.command_popup_dismissed {
            return false;
        }
        let Some(prefix) = active_command_prefix(&self.state.input) else {
            return false;
        };
        let matches = slash_command_matches(prefix);
        if matches.is_empty() {
            return false;
        }

        let selected_idx = if self.state.command_completion_prefix.as_deref() == Some(prefix) {
            self.state.command_completion_index.min(matches.len() - 1)
        } else {
            0
        };

        let command = matches[selected_idx].0;
        self.state.input.clear();
        self.state.input_cursor = 0;
        self.state.command_popup_dismissed = false;
        self.reset_completion_cycle();
        self.handle_command(command);
        true
    }

    pub(super) fn command_popup_visible(&self) -> bool {
        if self.state.command_popup_dismissed {
            return false;
        }

        active_command_prefix(&self.state.input)
            .map(slash_command_matches)
            .is_some_and(|matches| !matches.is_empty())
    }

    pub(super) fn handle_model_menu_key_event(&mut self, key_event: KeyEvent) {
        let models = self.flatten_models();
        let len = models.len();

        match key_event.code {
            KeyCode::Up => {
                if len > 0 {
                    self.state.model_menu_index =
                        model_menu_previous_index(self.state.model_menu_index, len);
                }
            }
            KeyCode::Down => {
                if len > 0 {
                    self.state.model_menu_index =
                        model_menu_next_index(self.state.model_menu_index, len);
                }
            }
            KeyCode::Enter => {
                if let Some((provider_id, model)) = models.get(self.state.model_menu_index) {
                    self.state.selected_provider_id = Some(provider_id.clone());
                    self.state.selected_model_id = Some(model.id.clone());
                    self.state.status = format!("Selected model: {} / {}", provider_id, model.name);
                    self.state.mode = UiMode::Chat;
                }
            }
            _ => {}
        }
    }

    pub(super) fn handle_agent_menu_key_event(&mut self, key_event: KeyEvent) {
        let len = self.state.agent_profiles.len();

        match key_event.code {
            KeyCode::Up => {
                if len > 0 {
                    self.state.agent_menu_index =
                        model_menu_previous_index(self.state.agent_menu_index, len);
                }
            }
            KeyCode::Down => {
                if len > 0 {
                    self.state.agent_menu_index =
                        model_menu_next_index(self.state.agent_menu_index, len);
                }
            }
            KeyCode::Enter => {
                if self.state.profile_locked {
                    self.state.status =
                        String::from("Cannot change agent while the current turn is active");
                    self.state.mode = UiMode::Chat;
                    return;
                }
                if let Some(profile) = self.state.agent_profiles.get(self.state.agent_menu_index) {
                    if let Some(session_id) = self.state.current_session_id.clone() {
                        self.request(RuntimeRequest::SetSessionProfile {
                            session_id,
                            profile_id: profile.id.clone(),
                        });
                    } else {
                        self.state.selected_profile_id = Some(profile.id.clone());
                        self.state.status = format!("Selected agent: {}", profile.id);
                        self.state.mode = UiMode::Chat;
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn handle_sessions_menu_key_event(&mut self, key_event: KeyEvent) {
        let total = self.state.sessions.len() + 1;

        match key_event.code {
            KeyCode::Up => {
                if total > 0 {
                    self.state.sessions_menu_index =
                        (self.state.sessions_menu_index + total - 1) % total;
                }
            }
            KeyCode::Down => {
                if total > 0 {
                    self.state.sessions_menu_index = (self.state.sessions_menu_index + 1) % total;
                }
            }
            KeyCode::Enter => {
                if self.state.sessions_menu_index == 0 {
                    self.start_new_chat();
                } else if let Some(session) = self
                    .state
                    .sessions
                    .get(self.state.sessions_menu_index.saturating_sub(1))
                {
                    self.request(RuntimeRequest::LoadSession {
                        session_id: session.id.clone(),
                    });
                }
            }
            KeyCode::Char('x') => {
                if self.state.sessions_menu_index > 0
                    && let Some(session) = self
                        .state
                        .sessions
                        .get(self.state.sessions_menu_index.saturating_sub(1))
                {
                    self.request(RuntimeRequest::DeleteSession {
                        session_id: session.id.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    pub(super) fn handle_tool_approval_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Left | KeyCode::BackTab => self.select_previous_tool_action(),
            KeyCode::Right | KeyCode::Tab => self.select_next_tool_action(),
            KeyCode::Enter => self.confirm_current_tool_action(),
            KeyCode::Char('a') => self.submit_tool_decision(true),
            KeyCode::Char('d') => self.submit_tool_decision(false),
            _ => {}
        }
    }

    pub(super) fn handle_providers_key_event(&mut self, key_event: KeyEvent) {
        if self.state.settings_editor.is_some() {
            self.handle_settings_editor_key_event(key_event);
            return;
        }

        match self.state.providers_view {
            ProvidersView::List => self.handle_provider_list_key_event(key_event),
            ProvidersView::Connect => self.handle_connect_provider_key_event(key_event),
            ProvidersView::Detail => self.handle_provider_detail_key_event(key_event),
            ProvidersView::Advanced => self.handle_advanced_provider_key_event(key_event),
        }
    }

    pub(super) fn handle_settings_editor_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Enter => self.commit_settings_editor(),
            KeyCode::Backspace => {
                self.state.settings_editor_input.pop();
            }
            KeyCode::Char(c) => {
                self.state.settings_editor_input.push(c);
            }
            _ => {}
        }
    }
}

fn is_ctrl_c(key_event: KeyEvent) -> bool {
    key_event.code == KeyCode::Char('c') && key_event.modifiers == KeyModifiers::CONTROL
}
