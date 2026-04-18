use super::*;

impl App {
    pub(super) fn handle_providers_escape(&mut self) {
        if self.state.settings_editor.take().is_some() {
            self.state.settings_editor_input.clear();
            return;
        }

        match self.state.providers_view {
            ProvidersView::List => {
                self.state.mode = UiMode::Chat;
                self.state.status = String::from("Providers closed");
            }
            ProvidersView::Connect => {
                self.state.providers_view = ProvidersView::List;
                self.state.connect_provider_search.clear();
                self.state.connect_provider_index = 0;
                self.state.status = String::from("Provider connection cancelled");
            }
            ProvidersView::Detail => {
                self.state.providers_view = ProvidersView::List;
                self.state.status = String::from("Back to providers");
            }
            ProvidersView::Advanced => {
                self.state.providers_view = ProvidersView::Detail;
                self.state.settings_delete_armed = false;
                self.state.status = String::from("Back to provider detail");
            }
        }
    }

    pub(super) fn handle_provider_list_key_event(&mut self, key_event: KeyEvent) {
        let len = self
            .state
            .settings_draft
            .as_ref()
            .map_or(0, |draft| draft.providers.len());

        match key_event.code {
            KeyCode::Up => {
                self.state.settings_provider_index =
                    adjust_index(self.state.settings_provider_index, len, -1);
                self.state.settings_model_index = 0;
                self.state.settings_delete_armed = false;
            }
            KeyCode::Down => {
                self.state.settings_provider_index =
                    adjust_index(self.state.settings_provider_index, len, 1);
                self.state.settings_model_index = 0;
                self.state.settings_delete_armed = false;
            }
            KeyCode::Enter => {
                if len == 0 {
                    self.state.status = String::from("No providers configured");
                    return;
                }
                self.state.providers_view = ProvidersView::Detail;
                self.maybe_request_openai_auth_status();
            }
            KeyCode::Char('a') => {
                self.state.providers_view = ProvidersView::Connect;
                self.state.connect_provider_search.clear();
                self.state.connect_provider_index = 0;
                self.state.status = String::from("Connect a provider");
            }
            KeyCode::Char('d') => self.delete_selected_provider_from_list(),
            KeyCode::Char('r') => {
                self.request(RuntimeRequest::ListModels);
                self.state.status = String::from("Refreshing provider models");
            }
            _ => {}
        }
    }

    pub(super) fn handle_connect_provider_key_event(&mut self, key_event: KeyEvent) {
        let len = self.filtered_provider_definitions().len();
        match key_event.code {
            KeyCode::Up => {
                self.state.connect_provider_index =
                    adjust_index(self.state.connect_provider_index, len, -1);
            }
            KeyCode::Down => {
                self.state.connect_provider_index =
                    adjust_index(self.state.connect_provider_index, len, 1);
            }
            KeyCode::Backspace => {
                self.state.connect_provider_search.pop();
                self.state.connect_provider_index = 0;
            }
            KeyCode::Char(c) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.connect_provider_search.push(c);
                self.state.connect_provider_index = 0;
            }
            KeyCode::Enter => self.create_provider_from_connect_selection(),
            _ => {}
        }
    }

    pub(super) fn handle_provider_detail_key_event(&mut self, key_event: KeyEvent) {
        if matches!(key_event.code, KeyCode::Backspace) {
            self.state.providers_view = ProvidersView::List;
            self.state.status = String::from("Back to providers");
            return;
        }

        match key_event.code {
            KeyCode::Char('b') => {
                self.execute_provider_detail_action(ProviderDetailAction::BrowserLogin)
            }
            KeyCode::Char('c') => {
                self.execute_provider_detail_action(ProviderDetailAction::DeviceCodeLogin)
            }
            KeyCode::Char('x') => {
                self.execute_provider_detail_action(ProviderDetailAction::CancelLogin)
            }
            KeyCode::Char('l') => self.execute_provider_detail_action(ProviderDetailAction::Logout),
            KeyCode::Char('e') => {
                self.execute_provider_detail_action(ProviderDetailAction::Advanced)
            }
            KeyCode::Char('r') => {
                self.execute_provider_detail_action(ProviderDetailAction::RefreshModels)
            }
            KeyCode::Char('o') => self.retry_open_pending_auth_target(),
            KeyCode::Char('y') => self.copy_pending_auth_value(),
            _ => {}
        }
    }

    pub(super) fn handle_advanced_provider_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Tab => self.cycle_advanced_focus(true),
            KeyCode::BackTab => self.cycle_advanced_focus(false),
            KeyCode::Up => self.move_advanced_selection(-1),
            KeyCode::Down => self.move_advanced_selection(1),
            KeyCode::Left => self.adjust_advanced_field(false),
            KeyCode::Right => self.adjust_advanced_field(true),
            KeyCode::Enter => self.activate_advanced_field(),
            KeyCode::Char('a') => self.add_advanced_item(),
            KeyCode::Char('d') => self.delete_advanced_item(),
            KeyCode::Char(' ') => self.toggle_settings_field(),
            KeyCode::Backspace => {
                self.state.providers_view = ProvidersView::Detail;
                self.state.status = String::from("Back to provider detail");
            }
            _ => {}
        }
    }

    pub(super) fn cycle_advanced_focus(&mut self, forward: bool) {
        self.state.settings_delete_armed = false;
        self.state.providers_advanced_focus = match (self.state.providers_advanced_focus, forward) {
            (ProvidersAdvancedFocus::ProviderFields, true) => ProvidersAdvancedFocus::Models,
            (ProvidersAdvancedFocus::Models, true) => ProvidersAdvancedFocus::ModelFields,
            (ProvidersAdvancedFocus::ModelFields, true) => ProvidersAdvancedFocus::ProviderFields,
            (ProvidersAdvancedFocus::ProviderFields, false) => ProvidersAdvancedFocus::ModelFields,
            (ProvidersAdvancedFocus::Models, false) => ProvidersAdvancedFocus::ProviderFields,
            (ProvidersAdvancedFocus::ModelFields, false) => ProvidersAdvancedFocus::Models,
        };
        self.sync_settings_focus_from_advanced();
    }

    pub(super) fn sync_settings_focus_from_advanced(&mut self) {
        self.state.settings_focus = match self.state.providers_advanced_focus {
            ProvidersAdvancedFocus::ProviderFields => SettingsFocus::ProviderForm,
            ProvidersAdvancedFocus::Models => SettingsFocus::ModelList,
            ProvidersAdvancedFocus::ModelFields => SettingsFocus::ModelForm,
        };
    }

    pub(super) fn move_advanced_selection(&mut self, delta: isize) {
        self.sync_settings_focus_from_advanced();
        self.move_settings_selection(delta);
    }

    pub(super) fn adjust_advanced_field(&mut self, forward: bool) {
        self.sync_settings_focus_from_advanced();
        self.adjust_settings_field(forward);
    }

    pub(super) fn activate_advanced_field(&mut self) {
        self.sync_settings_focus_from_advanced();
        self.activate_settings_field();
    }

    pub(super) fn add_advanced_item(&mut self) {
        self.sync_settings_focus_from_advanced();
        self.add_settings_item();
    }

    pub(super) fn delete_advanced_item(&mut self) {
        self.sync_settings_focus_from_advanced();
        self.delete_settings_item();
    }

    pub(super) fn delete_selected_provider_from_list(&mut self) {
        self.state.settings_focus = SettingsFocus::ProviderList;
        self.delete_settings_item();
    }

    pub(super) fn filtered_provider_definitions(&self) -> Vec<&ProviderDefinition> {
        let query = self
            .state
            .connect_provider_search
            .trim()
            .to_ascii_lowercase();
        let mut definitions: Vec<&ProviderDefinition> = self
            .state
            .provider_definitions
            .iter()
            .filter(|definition| {
                query.is_empty()
                    || definition
                        .display_name
                        .to_ascii_lowercase()
                        .contains(&query)
                    || definition.type_id.to_ascii_lowercase().contains(&query)
                    || definition.description.to_ascii_lowercase().contains(&query)
            })
            .collect();
        definitions.sort_by(|left, right| {
            provider_definition_rank(left)
                .cmp(&provider_definition_rank(right))
                .then_with(|| left.display_name.cmp(&right.display_name))
                .then_with(|| left.type_id.cmp(&right.type_id))
        });
        definitions
    }

    pub(super) fn create_provider_from_connect_selection(&mut self) {
        let definitions = self.filtered_provider_definitions();
        let Some((type_id, provider_fields, default_provider_id_prefix)) = definitions
            .get(self.state.connect_provider_index)
            .map(|definition| {
                (
                    definition.type_id.clone(),
                    definition.provider_fields.clone(),
                    definition.default_provider_id_prefix.clone(),
                )
            })
        else {
            self.state.status = String::from("No provider selected");
            return;
        };

        let Some(draft) = self.state.settings_draft.as_mut() else {
            self.state.status = String::from("Provider settings are not loaded yet");
            return;
        };

        let next_provider_id = next_provider_id(&draft.providers, &default_provider_id_prefix);
        draft.providers.push(ProviderSettings {
            id: next_provider_id.clone(),
            type_id,
            values: default_values(&provider_fields),
        });
        self.state.settings_provider_index = draft.providers.len().saturating_sub(1);
        self.state.settings_model_index = 0;
        self.state.settings_provider_field_index = 0;
        self.state.settings_model_field_index = 0;
        self.state.providers_view = ProvidersView::Detail;
        self.state.connect_provider_search.clear();
        self.state.connect_provider_index = 0;
        self.state.status = format!("Added provider: {next_provider_id}");
        self.save_settings_draft();
        self.maybe_request_openai_auth_status();
    }

    pub(super) fn execute_provider_detail_action(&mut self, action: ProviderDetailAction) {
        match action {
            ProviderDetailAction::BrowserLogin => {
                if self
                    .current_provider()
                    .is_some_and(|provider| provider.type_id == "openai-codex")
                {
                    self.request(RuntimeRequest::StartOpenAiCodexBrowserLogin);
                    self.state.status = String::from("Starting OpenAI browser login");
                }
            }
            ProviderDetailAction::DeviceCodeLogin => {
                if self
                    .current_provider()
                    .is_some_and(|provider| provider.type_id == "openai-codex")
                {
                    self.request(RuntimeRequest::StartOpenAiCodexDeviceCodeLogin);
                    self.state.status = String::from("Starting OpenAI device-code login");
                }
            }
            ProviderDetailAction::CancelLogin => {
                if self
                    .current_provider()
                    .is_some_and(|provider| provider.type_id == "openai-codex")
                {
                    self.request(RuntimeRequest::CancelOpenAiCodexLogin);
                    self.state.status = String::from("Cancelling OpenAI login");
                }
            }
            ProviderDetailAction::Logout => {
                if self
                    .current_provider()
                    .is_some_and(|provider| provider.type_id == "openai-codex")
                {
                    self.request(RuntimeRequest::LogoutOpenAiCodexAuth);
                    self.state.status = String::from("Logging out from OpenAI");
                }
            }
            ProviderDetailAction::Advanced => {
                self.state.providers_view = ProvidersView::Advanced;
                self.state.providers_advanced_focus = ProvidersAdvancedFocus::ProviderFields;
                self.state.settings_focus = SettingsFocus::ProviderForm;
                self.state.status = String::from("Advanced provider config");
            }
            ProviderDetailAction::RefreshModels => {
                self.request(RuntimeRequest::ListModels);
                self.state.status = String::from("Refreshing provider models");
            }
        }
    }

    pub(super) fn maybe_request_openai_auth_status(&self) {
        if self
            .current_provider()
            .is_some_and(|provider| provider.type_id == "openai-codex")
        {
            self.request(RuntimeRequest::GetOpenAiCodexAuthStatus);
        }
    }

    pub(super) fn apply_openai_codex_auth_status(&mut self, status: ProviderAuthStatus) {
        let previous_target = pending_auth_target(&self.state.openai_codex_auth).map(str::to_owned);
        let next_target = pending_auth_target(&status).map(str::to_owned);
        let next_state = status.state;
        self.state.openai_codex_auth = status;

        if let Some(target) = next_target
            && previous_target.as_deref() != Some(target.as_str())
        {
            self.state.status = match open_external_target(&target) {
                Ok(()) => match next_state {
                    ProviderAuthState::BrowserPending => {
                        String::from("Opened browser for OpenAI sign-in. Press y to copy the URL.")
                    }
                    ProviderAuthState::DeviceCodePending => String::from(
                        "Opened browser for OpenAI device sign-in. Press y to copy the code.",
                    ),
                    ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => {
                        String::from("Opened browser")
                    }
                },
                Err(err) => match next_state {
                    ProviderAuthState::BrowserPending => {
                        format!("Failed to open browser: {err}. Press y to copy the sign-in URL.")
                    }
                    ProviderAuthState::DeviceCodePending => {
                        format!("Failed to open browser: {err}. Press y to copy the device code.")
                    }
                    ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => {
                        format!("Failed to open browser: {err}")
                    }
                },
            };
        }
    }

    pub(super) fn retry_open_pending_auth_target(&mut self) {
        let target = pending_auth_target(&self.state.openai_codex_auth).map(str::to_owned);

        let Some(target) = target else {
            self.state.status = String::from("No pending auth URL available");
            return;
        };

        self.state.status = match open_external_target(&target) {
            Ok(()) => String::from("Opened browser"),
            Err(err) => format!("Failed to open browser: {err}"),
        };
    }

    pub(super) fn copy_pending_auth_value(&mut self) {
        let value = match self.state.openai_codex_auth.state {
            ProviderAuthState::BrowserPending => self.state.openai_codex_auth.auth_url.clone(),
            ProviderAuthState::DeviceCodePending => self.state.openai_codex_auth.user_code.clone(),
            ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => None,
        };

        let Some(value) = value else {
            self.state.status = String::from("Nothing to copy");
            return;
        };

        self.state.status = match self.copy_text_to_clipboard(&value) {
            Ok(()) => String::from("Copied to clipboard"),
            Err(err) => format!("Copy failed: {err}"),
        };
    }
}
