use super::*;

impl AppState {
    pub(super) fn find_model(&self, provider_id: &str, model_id: &str) -> Option<&Model> {
        self.models_by_provider
            .get(provider_id)?
            .iter()
            .find(|model| model.id == model_id)
    }

    pub(super) fn is_current_model(&self, provider_id: &str, model_id: &str) -> bool {
        self.selected_provider_id.as_deref() == Some(provider_id)
            && self.selected_model_id.as_deref() == Some(model_id)
    }

    /// The model the model list highlights.
    pub(super) fn highlighted_model(&self) -> Option<(&str, &Model)> {
        let models = self.filtered_models();
        let index = self.sheet_index.min(models.len().checked_sub(1)?);
        models.get(index).copied()
    }

    fn model_position(&self, provider_id: &str, model_id: &str) -> Option<usize> {
        self.filtered_models()
            .iter()
            .position(|(provider, model)| *provider == provider_id && model.id == model_id)
    }
}

impl App {
    pub(super) fn open_model_sheet(&mut self, query: String) {
        self.state.sheet_query = query;
        self.state.sheet_notice = None;
        self.state.model_settings = None;
        self.state.mode = UiMode::Model;
        self.state.reset_sheet_selection();
        if self.state.sheet_query.is_empty()
            && let Some((provider_id, model_id)) = self
                .state
                .selected_provider_id
                .as_deref()
                .zip(self.state.selected_model_id.as_deref())
        {
            self.state.sheet_index = self
                .state
                .model_position(provider_id, model_id)
                .unwrap_or(0);
        }
        self.settle_highlighted_model();
        self.request(RuntimeRequest::ListModels);
    }

    /// Returns to the model list with `provider_id` / `model_id` highlighted.
    pub(super) fn return_to_model_sheet(&mut self, provider_id: &str, model_id: &str) {
        self.state.sheet_query.clear();
        self.state.sheet_notice = None;
        self.state.model_settings = None;
        self.state.mode = UiMode::Model;
        self.state.reset_sheet_selection();
        self.state.sheet_index = self
            .state
            .model_position(provider_id, model_id)
            .unwrap_or(0);
        self.settle_highlighted_model();
    }

    pub(super) fn close_sheet(&mut self) {
        self.state.mode = UiMode::Chat;
        self.state.sheet_query.clear();
        self.state.sheet_notice = None;
        self.state.model_settings = None;
        self.reset_command_selection();
    }

    /// Closes the sheet and puts `command` back in an empty composer, as if backspacing into it.
    pub(super) fn close_sheet_to_composer(&mut self, command: &str) {
        self.close_sheet();
        if self.state.input.is_empty() && self.state.draft_images.len() == 0 {
            self.set_input_text(command.to_owned());
            self.state.command_popup_dismissed = false;
        }
    }

    pub(super) fn handle_model_key_event(&mut self, key: KeyEvent) {
        self.step_model_sheet(key);
        self.settle_highlighted_model();
    }

    fn step_model_sheet(&mut self, key: KeyEvent) {
        self.state.sheet_notice = None;
        if key.code == KeyCode::Backspace
            && key.modifiers.is_empty()
            && self.state.sheet_query.is_empty()
        {
            self.close_sheet_to_composer("/model");
            return;
        }
        if self.edit_sheet_query(key) {
            self.state.reset_sheet_selection();
            return;
        }
        let len = self.state.filtered_models().len();
        let index = self.state.sheet_index.min(len.saturating_sub(1));
        let selected = self
            .state
            .highlighted_model()
            .map(|(provider, model)| (provider.to_owned(), model.id.clone(), model.name.clone()));
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                self.state.sheet_index = model_menu_previous_index(index, len)
            }
            KeyCode::Down | KeyCode::Tab => {
                self.state.sheet_index = model_menu_next_index(index, len)
            }
            KeyCode::Left | KeyCode::Right => {
                if let Some((provider_id, model_id, _)) = selected {
                    self.step_inline_option(&provider_id, &model_id, key.code == KeyCode::Right);
                }
            }
            KeyCode::Char('s') if key.modifiers == KeyModifiers::CONTROL => {
                if let Some((provider_id, model_id, name)) = selected {
                    if self.state.model_has_more_settings(&provider_id, &model_id) {
                        self.open_model_settings(provider_id, model_id);
                    } else {
                        self.state.sheet_notice = Some(format!("{name} has no other settings"));
                    }
                }
            }
            KeyCode::Enter => {
                if let Some((provider_id, model_id, _)) = selected {
                    self.select_model(&provider_id, &model_id);
                }
            }
            _ => {}
        }
    }

    /// Switches to a model, bringing back the option values last used with it.
    pub(super) fn select_model(&mut self, provider_id: &str, model_id: &str) {
        let Some(model) = self.state.find_model(provider_id, model_id) else {
            return;
        };
        let status = format!("Selected model: {provider_id} / {}", model.name);
        let values = self.state.model_option_values(provider_id, model_id);
        self.remember_model_options();
        self.state.selected_provider_id = Some(provider_id.to_owned());
        self.state.selected_model_id = Some(model_id.to_owned());
        self.state.selected_model_options = values;
        self.state.status = status;
        self.save_model_selection();
        self.close_sheet();
    }
}
