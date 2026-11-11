use super::*;

impl App {
    pub(super) fn handle_submit(&mut self) {
        let raw_input = self.state.input.trim().to_string();
        if raw_input.is_empty() && self.state.draft_images.len() == 0 {
            return;
        }

        let command_popup_dismissed = self.state.command_popup_dismissed;
        self.state.command_popup_dismissed = false;

        if self.state.draft_images.len() == 0
            && !command_popup_dismissed
            && let Some(command) = raw_input.strip_prefix('/')
            && is_known_slash_command(command)
        {
            self.handle_command(command.trim());
            return;
        }

        self.submit_message(raw_input);
    }

    pub(super) fn submit_message(&mut self, raw_input: String) {
        if self.state.draft_images.pending() {
            self.state.status = String::from("Wait for the image attachment to finish loading");
            return;
        }
        if raw_input.trim().is_empty() && self.state.draft_images.len() == 0 {
            let message = String::from("Message cannot be empty");
            self.state.status = message.clone();
            return;
        }

        let Some((model_id, provider_id)) = self.submission_model() else {
            return;
        };

        let is_queueing = self.state.is_streaming
            || self.state.script_phase == ScriptPhase::Executing
            || self.state.pending_script.is_some();

        let message = self.compose_message(&raw_input);
        if message.images().count() > kraai_execution_types::image::MAX_IMAGE_ATTACHMENTS {
            self.state.status =
                String::from("Too many images. Delete an image chip before sending");
            return;
        }

        if let Some(session_id) = self.state.current_session_id.clone() {
            self.dispatch_send_message(
                session_id,
                message,
                model_id,
                provider_id,
                self.state.selected_model_options.clone(),
                types::SubmissionSource::Composer {
                    queued: is_queueing,
                },
            );
            return;
        }

        if self.state.pending_submit.is_some() {
            self.state.status =
                String::from("Session creation already in progress; message was not sent");
            return;
        }

        let creation_id = self.state.next_session_creation_id;
        self.state.next_session_creation_id = creation_id.wrapping_add(1);
        if self.request(RuntimeRequest::CreateSession {
            creation_id,
            profile_id: self.state.selected_profile_id.clone(),
        }) == RuntimeRequestDelivery::Disconnected
        {
            return;
        }
        self.state.pending_submit = Some(PendingSubmit {
            creation_id,
            session_id: None,
            message,
            model_id,
            provider_id,
            options: self.state.selected_model_options.clone(),
        });
        self.clear_message_draft();
        self.state.status = String::from("Creating session");
    }

    fn submission_model(&mut self) -> Option<(String, String)> {
        if !self.state.config_loaded {
            self.set_error(String::from("Config not loaded yet"));
            return None;
        }

        let Some(provider_id) = self.state.selected_provider_id.clone() else {
            self.set_error(String::from("No provider selected. Use /model"));
            return None;
        };
        let Some(model_id) = self.state.selected_model_id.clone() else {
            self.set_error(String::from("No model selected. Use /model"));
            return None;
        };
        if self.state.selected_profile_id.is_none() {
            self.set_error(String::from("No agent selected. Use /agent"));
            return None;
        }

        if let Some(model) = self.state.selected_model()
            && let Err(errors) = kraai_types::validate_model_options(
                &model.options,
                &self.state.selected_model_options,
            )
        {
            let message = errors
                .into_iter()
                .map(|error| format!("{}: {}", error.option, error.message))
                .collect::<Vec<_>>()
                .join("; ");
            self.set_error(format!("Choose model options in /model: {message}"));
            return None;
        }
        Some((model_id, provider_id))
    }

    pub(super) fn handle_command(&mut self, command_line: &str) {
        let mut parts = command_line.split_whitespace();
        let Some(command) = parts.next() else {
            self.state.status = String::from("Empty command");
            return;
        };

        match command {
            "quit" => {
                self.state.exit = true;
            }
            "model" => {
                self.set_input_text(String::new());
                self.open_model_sheet(parts.collect::<Vec<_>>().join(" "));
            }
            "error" => {
                self.set_input_text(String::new());
                self.open_error();
            }
            "agent" => {
                self.set_input_text(String::new());
                self.open_agent_sheet();
            }
            "providers" => {
                self.set_input_text(String::new());
                self.open_providers_sheet();
            }
            "mcp" => self.handle_mcp_command(parts.collect()),
            "sessions" => {
                self.set_input_text(String::new());
                self.open_sessions_sheet();
            }
            "new" => {
                self.start_new_chat();
            }
            "undo" => {
                let Some(session_id) = self.state.current_session_id.clone() else {
                    self.state.status = String::from("No session to undo");
                    return;
                };
                if self.state.turn_blocks_user_commands() {
                    self.state.status =
                        String::from("Cannot undo while the current turn is active");
                    return;
                }
                self.request(RuntimeRequest::UndoLastUserMessage { session_id });
            }
            _ => {
                self.state.status = format!("Unknown command: /{command}");
            }
        }
    }

    pub(super) fn ensure_selected_model(&mut self) {
        if self.state.selected_model_id.is_some() {
            return;
        }
        let previous_provider_id = self.state.selected_provider_id.clone();
        let selection = match previous_provider_id.as_ref() {
            Some(provider_id) => self
                .state
                .models_by_provider
                .get(provider_id)
                .and_then(|models| models.first())
                .map(|model| (provider_id, model)),
            None => self
                .state
                .models_by_provider
                .iter()
                .filter_map(|(provider, models)| models.first().map(|model| (provider, model)))
                .min_by(|(left, _), (right, _)| left.cmp(right)),
        };
        let Some((provider_id, model)) = selection else {
            return;
        };
        self.state.selected_provider_id = Some(provider_id.clone());
        self.state.selected_model_id = Some(model.id.clone());
        self.save_workspace_preferences_if_model_changed(previous_provider_id, None);
    }

    pub(super) fn set_input_text(&mut self, text: String) {
        self.reset_input_history_navigation();
        self.state.command_filter = None;
        self.state.draft_images = draft_images::DraftImages::default();
        self.state.input = text;
        self.state.input_cursor = self.state.input.len();
    }

    pub(super) fn maybe_send_startup_message(&mut self) {
        if self.startup_message_sent {
            return;
        }

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

        let Some(message) = self.startup_options.message.take() else {
            return;
        };

        self.startup_message_sent = true;
        self.submit_message(message);
    }

    pub(super) fn save_workspace_preferences(&mut self) {
        if !self.persist_workspace_preferences {
            return;
        }
        let preferences = WorkspacePreferences {
            provider_id: self.state.selected_provider_id.clone(),
            model_id: self.state.selected_model_id.clone(),
            agent_profile_id: self.state.selected_profile_id.clone(),
            model_options: self.state.remembered_model_options.clone(),
        };

        if let Err(error) = workspace_preferences::save_for_current_workspace(&preferences) {
            tracing::warn!("Failed to save workspace preferences: {error}");
            self.state.status = format!("Failed saving workspace preferences: {error}");
        }
    }

    fn save_workspace_preferences_if_model_changed(
        &mut self,
        previous_provider_id: Option<String>,
        previous_model_id: Option<String>,
    ) {
        if previous_provider_id != self.state.selected_provider_id
            || previous_model_id != self.state.selected_model_id
        {
            self.reconcile_model_options();
            self.save_workspace_preferences();
        }
    }

    pub(super) fn apply_agent_profiles_state(&mut self, state: AgentProfilesState) {
        self.state.agent_profiles = state.profiles;
        self.state.agent_profile_warnings = state.warnings;
        self.state.selected_profile_id = state.selected_profile_id;
        self.state.profile_locked = state.profile_locked;
        self.state.profile_lock_stale_after_terminal_event = false;
        self.state.highlight_selected_agent();
        if let Some(warning) = self.state.agent_profile_warnings.first() {
            self.state.status = format!("Agent profile warning: {}", warning.message);
        }
    }

    pub(super) fn sync_current_session_profile_from_sessions(&mut self) {
        let Some(session_id) = self.state.current_session_id.as_ref() else {
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

    pub(super) fn sync_current_session_streaming_from_sessions(&mut self) {
        let Some(session_id) = self.state.current_session_id.as_ref() else {
            self.state.is_streaming = false;
            return;
        };
        if let Some(session) = self
            .state
            .sessions
            .iter()
            .find(|session| &session.id == session_id)
        {
            self.state.is_streaming = session.is_streaming;
        }
    }
}
