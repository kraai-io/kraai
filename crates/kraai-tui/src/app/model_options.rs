use kraai_types::{ModelOptionDefinition, ModelOptionKind};

use super::*;

pub(super) fn model_option_choices(option: &ModelOptionDefinition) -> Vec<(String, String)> {
    let mut choices: Vec<_> = match &option.kind {
        ModelOptionKind::Choice { choices } => choices
            .iter()
            .map(|choice| (choice.id.clone(), choice.label.clone()))
            .collect(),
        ModelOptionKind::Boolean { .. } => ["false", "true"]
            .into_iter()
            .map(|value| (value.to_owned(), value.to_owned()))
            .collect(),
        ModelOptionKind::Integer { .. } => return Vec::new(),
    };
    if !option.required {
        choices.insert(0, (String::new(), String::from("Unset")));
    }
    choices
}

impl AppState {
    pub(super) fn selected_model(&self) -> Option<&Model> {
        self.models_by_provider
            .get(self.selected_provider_id.as_ref()?)?
            .iter()
            .find(|model| Some(&model.id) == self.selected_model_id.as_ref())
    }

    pub(super) fn active_model_options(&self) -> Vec<&ModelOptionDefinition> {
        self.selected_model()
            .into_iter()
            .flat_map(|model| &model.options)
            .filter(|option| option.is_active(&self.selected_model_options))
            .collect()
    }
}

impl App {
    pub(super) fn apply_startup_model_options(&mut self) -> Result<(), String> {
        if self.state.startup_model_options_applied {
            return Ok(());
        }
        let Some(model) = self.state.selected_model() else {
            return Ok(());
        };
        let values = kraai_types::parse_model_option_assignments(
            &model.options,
            &self.startup_options.options,
            self.state.selected_model_options.clone(),
        )?;
        self.state.selected_model_options = values;
        self.state.startup_model_options_applied = true;
        Ok(())
    }

    pub(super) fn reconcile_model_options(&mut self) {
        let (Some(provider_id), Some(model_id)) = (
            &self.state.selected_provider_id,
            &self.state.selected_model_id,
        ) else {
            return;
        };
        let Some(model) = self
            .state
            .models_by_provider
            .get(provider_id)
            .and_then(|models| models.iter().find(|model| &model.id == model_id))
        else {
            return;
        };
        kraai_types::reconcile_model_option_values(
            &model.options,
            &mut self.state.selected_model_options,
        );
    }

    pub(super) fn set_model_option(&mut self, id: &str, input: &str) -> Result<(), String> {
        let model = self
            .state
            .selected_model()
            .ok_or_else(|| String::from("Select a model first"))?;
        let values = kraai_types::parse_model_option_assignments(
            &model.options,
            &[format!("{id}={input}")],
            self.state.selected_model_options.clone(),
        )?;
        self.state.selected_model_options = values;
        self.state.status = if input.is_empty() {
            format!("Unset {id}")
        } else {
            format!("Selected {id}: {input}")
        };
        self.save_model_selection();
        Ok(())
    }

    pub(super) fn open_model_options(&mut self) {
        self.state.option_menu_index = 0;
        self.state.option_choice_index = 0;
        self.state.option_editing = false;
        self.state.option_editor_input.clear();
        self.state.mode = UiMode::ModelOptionsMenu;
    }

    pub(super) fn handle_option_command(&mut self, parts: Vec<&str>) {
        match parts.as_slice() {
            [] => self.open_model_options(),
            [id, value] => {
                match self.set_model_option(id, if *value == "--clear" { "" } else { value }) {
                    Ok(()) => {
                        self.set_input_text(String::new());
                    }
                    Err(error) => self.state.status = error,
                }
            }
            _ => self.state.status = String::from("Usage: /option [<id> <value|--clear>]"),
        }
    }

    pub(super) fn handle_model_options_key_event(&mut self, key: KeyEvent) {
        let definitions = self.state.active_model_options();
        let len = definitions.len();
        let Some(option) = definitions
            .get(self.state.option_menu_index)
            .map(|option| (*option).clone())
        else {
            return;
        };
        if !self.state.option_editing {
            match key.code {
                KeyCode::Up => {
                    self.state.option_menu_index =
                        model_menu_previous_index(self.state.option_menu_index, len)
                }
                KeyCode::Down => {
                    self.state.option_menu_index =
                        model_menu_next_index(self.state.option_menu_index, len)
                }
                KeyCode::Enter => {
                    self.state.option_editing = true;
                    self.state.option_editor_input = self
                        .state
                        .selected_model_options
                        .get(&option.id)
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    self.state.option_choice_index = model_option_choices(&option)
                        .iter()
                        .position(|(value, _)| value == &self.state.option_editor_input)
                        .unwrap_or(0);
                }
                _ => {}
            }
            return;
        }
        let choices = model_option_choices(&option);
        match key.code {
            KeyCode::Up if !choices.is_empty() => {
                self.state.option_choice_index =
                    model_menu_previous_index(self.state.option_choice_index, choices.len())
            }
            KeyCode::Down if !choices.is_empty() => {
                self.state.option_choice_index =
                    model_menu_next_index(self.state.option_choice_index, choices.len())
            }
            KeyCode::Char(ch) if choices.is_empty() && (ch.is_ascii_digit() || ch == '-') => {
                self.state.option_editor_input.push(ch)
            }
            KeyCode::Backspace if choices.is_empty() => {
                self.state.option_editor_input.pop();
            }
            KeyCode::Enter => {
                let input = choices
                    .get(self.state.option_choice_index)
                    .map(|(value, _)| value.clone())
                    .unwrap_or_else(|| self.state.option_editor_input.clone());
                match self.set_model_option(&option.id, &input) {
                    Ok(()) => {
                        self.state.option_editing = false;
                        self.state.option_menu_index = 0;
                    }
                    Err(error) => self.state.status = error,
                }
            }
            _ => {}
        }
    }
}
