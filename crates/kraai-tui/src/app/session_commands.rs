use super::*;

impl App {
    pub(super) fn handle_submit(&mut self) {
        let raw_input = self.state.input.trim().to_string();
        if raw_input.is_empty() {
            return;
        }

        let command_popup_dismissed = self.state.command_popup_dismissed;
        self.state.command_popup_dismissed = false;

        if !command_popup_dismissed
            && let Some(command) = raw_input.strip_prefix('/')
            && (is_known_slash_command(command) || command.trim() == "settings")
        {
            self.handle_command(command.trim());
            return;
        }

        self.state.input.clear();
        self.state.input_cursor = 0;

        self.submit_message(raw_input);
    }

    pub(super) fn submit_message(&mut self, raw_input: String) {
        if raw_input.trim().is_empty() {
            let message = String::from("Message cannot be empty");
            self.state.status = message.clone();
            self.fail_ci(message);
            return;
        }

        if !self.state.config_loaded {
            self.state.status = String::from("Config not loaded yet");
            return;
        }

        let Some(provider_id) = self.state.selected_provider_id.clone() else {
            self.state.status = String::from("No provider selected. Use /model");
            return;
        };
        let Some(model_id) = self.state.selected_model_id.clone() else {
            self.state.status = String::from("No model selected. Use /model");
            return;
        };
        if self.state.selected_profile_id.is_none() {
            self.state.status = String::from("No agent selected. Use /agent");
            return;
        }

        let is_queueing = self.state.is_streaming
            || self.state.tool_phase == ToolPhase::ExecutingBatch
            || !self.state.pending_tools.is_empty();

        if let Some(session_id) = self.state.current_session_id.clone() {
            self.dispatch_send_message(session_id, raw_input, model_id, provider_id, is_queueing);
            return;
        }

        self.state.pending_submit = Some(PendingSubmit {
            session_id: None,
            message: raw_input,
            model_id,
            provider_id,
        });
        self.state.status = String::from("Creating session");
        self.request(RuntimeRequest::CreateSession);
    }

    pub(super) fn handle_command(&mut self, command_line: &str) {
        let mut parts = command_line.split_whitespace();
        let Some(command) = parts.next() else {
            self.state.status = String::from("Empty command. Use /help");
            return;
        };

        match command {
            "quit" => {
                self.state.exit = true;
            }
            "model" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::ModelMenu;
                self.request(RuntimeRequest::ListModels);
            }
            "agent" => {
                if self.state.profile_locked {
                    self.state.status =
                        String::from("Cannot change agent while the current turn is active");
                    return;
                }
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::AgentMenu;
                if let Some(session_id) = self.state.current_session_id.clone() {
                    self.request(RuntimeRequest::ListAgentProfiles { session_id });
                } else {
                    self.state.agent_profiles = default_agent_profiles();
                    self.state.agent_profile_warnings.clear();
                    if let Some(selected_profile_id) = self.state.selected_profile_id.as_ref()
                        && let Some(index) = self
                            .state
                            .agent_profiles
                            .iter()
                            .position(|profile| &profile.id == selected_profile_id)
                    {
                        self.state.agent_menu_index = index;
                    } else {
                        self.state.agent_menu_index = 0;
                    }
                }
            }
            "settings" => {
                self.state.status = String::from("Unknown command: /settings. Use /providers");
            }
            "providers" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::ProvidersMenu;
                self.state.providers_view = ProvidersView::List;
                self.state.status = String::from("Loading providers");
                self.request(RuntimeRequest::ListProviderDefinitions);
                self.request(RuntimeRequest::GetSettings);
                self.request(RuntimeRequest::GetOpenAiCodexAuthStatus);
            }
            "sessions" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::SessionsMenu;
                self.request(RuntimeRequest::ListSessions);
            }
            "new" => {
                self.start_new_chat();
            }
            "undo" => {
                let Some(session_id) = self.state.current_session_id.clone() else {
                    self.state.status = String::from("No session to undo");
                    return;
                };
                if self.state.is_streaming || self.state.retry_waiting || self.state.profile_locked
                {
                    self.state.status =
                        String::from("Cannot undo while the current turn is active");
                    return;
                }
                self.request(RuntimeRequest::UndoLastUserMessage { session_id });
            }
            "continue" => {
                let Some(session_id) = self.state.current_session_id.clone() else {
                    self.state.status = String::from("No session to continue");
                    return;
                };
                if self.state.is_streaming || self.state.retry_waiting || self.state.profile_locked
                {
                    self.state.status =
                        String::from("Cannot continue while the current turn is active");
                    return;
                }
                self.request(RuntimeRequest::ContinueSession { session_id });
            }
            "help" => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.mode = UiMode::Help;
            }
            _ => {
                self.state.status = format!("Unknown command: /{command}. Use /help");
            }
        }
    }

    pub(super) fn ensure_selected_model(&mut self) {
        if let Some(provider_id) = self.state.selected_provider_id.as_ref()
            && let Some(models) = self.state.models_by_provider.get(provider_id)
        {
            if let Some(model_id) = self.state.selected_model_id.as_ref()
                && models.iter().any(|model| &model.id == model_id)
            {
                return;
            }

            if let Some(model) = models.first() {
                self.state.selected_model_id = Some(model.id.clone());
                return;
            }
        }

        if let Some(model_id) = self.state.selected_model_id.as_ref()
            && let Some((provider_id, _)) = self
                .state
                .models_by_provider
                .iter()
                .find(|(_, models)| models.iter().any(|model| &model.id == model_id))
        {
            self.state.selected_provider_id = Some(provider_id.clone());
            return;
        }

        if let Some((provider_id, models)) = self.state.models_by_provider.iter().next()
            && let Some(model) = models.first()
        {
            self.state.selected_provider_id = Some(provider_id.clone());
            self.state.selected_model_id = Some(model.id.clone());
        }
    }

    pub(super) fn set_input_text(&mut self, text: String) {
        self.state.input = text;
        self.state.input_cursor = self.state.input.len();
    }

    pub(super) fn maybe_send_startup_message(&mut self) {
        if self.startup_message_sent {
            return;
        }

        let Some(message) = self.startup_options.message.clone() else {
            return;
        };

        if !self.state.config_loaded
            || self.state.pending_submit.is_some()
            || self.state.is_streaming
        {
            return;
        }

        if self.state.selected_provider_id.is_none()
            || self.state.selected_model_id.is_none()
            || self.state.selected_profile_id.is_none()
        {
            return;
        }

        self.startup_message_sent = true;
        self.submit_message(message);
    }

    pub(super) fn validate_ci_model_selection(&self) -> std::result::Result<(), String> {
        let provider_id = self
            .startup_options
            .provider_id
            .as_ref()
            .ok_or_else(|| String::from("CI mode requires --provider"))?;
        let model_id = self
            .startup_options
            .model_id
            .as_ref()
            .ok_or_else(|| String::from("CI mode requires --model"))?;

        let Some(models) = self.state.models_by_provider.get(provider_id) else {
            return Err(format!("Unknown provider for --ci: {provider_id}"));
        };

        if models.iter().any(|model| &model.id == model_id) {
            Ok(())
        } else {
            Err(format!(
                "Unknown model for provider {provider_id}: {model_id}"
            ))
        }
    }

    pub(super) fn is_ci_mode(&self) -> bool {
        self.startup_options.ci
    }

    pub(super) fn fail_ci(&mut self, message: impl Into<String>) {
        if !self.is_ci_mode() || self.state.exit {
            return;
        }

        let message = message.into();
        self.finish_ci_output_line();
        self.ci_turn_completion_pending = false;
        self.ci_error = Some(message.clone());
        self.state.status = message;
        self.state.is_streaming = false;
        self.state.exit = true;
    }

    pub(super) fn maybe_finish_ci_run(&mut self) {
        if !self.is_ci_mode() || !self.ci_turn_completion_pending || self.state.exit {
            return;
        }

        if self.state.is_streaming
            || !self.state.pending_tools.is_empty()
            || self.state.tool_phase != ToolPhase::Idle
            || self.state.profile_locked
        {
            return;
        }

        self.ci_turn_completion_pending = false;
        self.state.status = String::from("CI run completed");
        self.state.exit = true;
    }

    pub(super) fn write_ci_output(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }

        let _ = self.ci_output.write_all(chunk.as_bytes());
        let _ = self.ci_output.flush();
        self.ci_output_needs_newline = !chunk.ends_with('\n');
    }

    pub(super) fn finish_ci_output_line(&mut self) {
        if !self.ci_output_needs_newline {
            return;
        }

        let _ = self.ci_output.write_all(b"\n");
        let _ = self.ci_output.flush();
        self.ci_output_needs_newline = false;
    }

    pub(super) fn apply_agent_profiles_state(&mut self, state: AgentProfilesState) {
        self.state.agent_profiles = state.profiles;
        self.state.agent_profile_warnings = state.warnings;
        self.state.selected_profile_id = state.selected_profile_id;
        self.state.profile_locked = state.profile_locked;
        if let Some(selected_profile_id) = self.state.selected_profile_id.as_ref()
            && let Some(index) = self
                .state
                .agent_profiles
                .iter()
                .position(|profile| &profile.id == selected_profile_id)
        {
            self.state.agent_menu_index = index;
        } else {
            self.state.agent_menu_index = 0;
        }
        if let Some(warning) = self.state.agent_profile_warnings.first() {
            self.state.status = format!("Agent profile warning: {}", warning.message);
        }
    }

    pub(super) fn sync_current_session_profile_from_sessions(&mut self) {
        let Some(session_id) = self.state.current_session_id.as_ref() else {
            self.state
                .selected_profile_id
                .get_or_insert_with(|| String::from(DEFAULT_AGENT_PROFILE_ID));
            self.state.profile_locked = false;
            return;
        };
        if let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| &session.id == session_id)
        {
            self.state.selected_profile_id = session.selected_profile_id.clone();
            self.state.profile_locked = session.profile_locked;
        }
    }

    pub(super) fn flatten_models(&self) -> Vec<(String, Model)> {
        flatten_models_map(&self.state.models_by_provider)
    }
}
