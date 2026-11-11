use kraai_types::{
    ModelOptionDefinition, ModelOptionKind, ModelOptionValue, ModelOptionValues,
    REASONING_EFFORT_OPTION_ID,
};

use super::*;

/// The model whose settings the settings sheet edits.
#[derive(Clone)]
pub(super) struct ModelSettingsTarget {
    pub(super) provider_id: String,
    pub(super) model_id: String,
}

pub(super) fn model_option_choices(option: &ModelOptionDefinition) -> Vec<(String, String)> {
    let mut choices: Vec<_> = match &option.kind {
        ModelOptionKind::Choice { choices } => choices
            .iter()
            .map(|choice| (choice.id.clone(), choice.label.clone()))
            .collect(),
        ModelOptionKind::Boolean { .. } => [("false", "off"), ("true", "on")]
            .into_iter()
            .map(|(value, label)| (value.to_owned(), label.to_owned()))
            .collect(),
        ModelOptionKind::Integer { .. } => return Vec::new(),
    };
    if !option.required {
        choices.insert(0, (String::new(), String::from("unset")));
    }
    choices
}

/// The position of the value in effect within `model_option_choices`.
pub(super) fn chosen_choice(
    choices: &[(String, String)],
    value: Option<&ModelOptionValue>,
) -> Option<usize> {
    let current = value.map(ToString::to_string).unwrap_or_default();
    choices.iter().position(|(id, _)| *id == current)
}

/// The choice one step left or right of the value in effect, wrapping at the ends.
fn stepped_choice(
    option: &ModelOptionDefinition,
    value: Option<&ModelOptionValue>,
    forward: bool,
) -> Option<String> {
    let choices = model_option_choices(option);
    let next = match (chosen_choice(&choices, value), forward) {
        (Some(position), false) => model_menu_previous_index(position, choices.len()),
        (Some(position), true) => model_menu_next_index(position, choices.len()),
        (None, false) => choices.len().saturating_sub(1),
        (None, true) => 0,
    };
    choices.into_iter().nth(next).map(|(value, _)| value)
}

/// The reasoning effort option of a model while it is active. The model list changes it in
/// place on the highlighted row.
pub(super) fn inline_option<'a>(
    model: &'a Model,
    values: &ModelOptionValues,
) -> Option<&'a ModelOptionDefinition> {
    model.options.iter().find(|option| {
        option.id == REASONING_EFFORT_OPTION_ID
            && matches!(option.kind, ModelOptionKind::Choice { .. })
            && option.is_active(values)
    })
}

pub(super) fn active_options<'a>(
    model: &'a Model,
    values: &'a ModelOptionValues,
) -> impl Iterator<Item = &'a ModelOptionDefinition> {
    model
        .options
        .iter()
        .filter(move |option| option.is_active(values))
}

pub(super) fn option_value_label(
    option: &ModelOptionDefinition,
    value: Option<&ModelOptionValue>,
) -> String {
    match value {
        Some(ModelOptionValue::Choice(id)) => match &option.kind {
            ModelOptionKind::Choice { choices } => choices
                .iter()
                .find(|choice| &choice.id == id)
                .map_or_else(|| id.clone(), |choice| choice.label.clone()),
            _ => id.clone(),
        },
        Some(ModelOptionValue::Boolean(true)) => String::from("on"),
        Some(ModelOptionValue::Boolean(false)) => String::from("off"),
        Some(ModelOptionValue::Integer(value)) => value.to_string(),
        None if option.required => String::from("choose a value"),
        None => String::from("unset"),
    }
}

/// The values in effect for a model as short words, such as "High, thinking, budget 50".
pub(super) fn option_summary(model: &Model, values: &ModelOptionValues) -> String {
    summarize_options(active_options(model, values), values)
}

pub(super) fn summarize_options<'a>(
    options: impl Iterator<Item = &'a ModelOptionDefinition>,
    values: &ModelOptionValues,
) -> String {
    options
        .filter_map(|option| match values.get(&option.id)? {
            ModelOptionValue::Boolean(true) => Some(option.label.to_lowercase()),
            ModelOptionValue::Boolean(false) => None,
            ModelOptionValue::Integer(value) => {
                Some(format!("{} {value}", option.label.to_lowercase()))
            }
            value @ ModelOptionValue::Choice(_) => Some(option_value_label(option, Some(value))),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Turns typed text into the value the option parser takes; `None` when it names no value.
fn resolve_option_input(option: &ModelOptionDefinition, raw: &str) -> Option<String> {
    let resolved = match &option.kind {
        ModelOptionKind::Choice { choices } => choices
            .iter()
            .find(|choice| choice.id == raw || choice.label.eq_ignore_ascii_case(raw))
            .map(|choice| choice.id.clone()),
        ModelOptionKind::Boolean { .. } => match raw.to_ascii_lowercase().as_str() {
            "on" | "true" | "yes" => Some(String::from("true")),
            "off" | "false" | "no" => Some(String::from("false")),
            _ => None,
        },
        ModelOptionKind::Integer { .. } => raw.parse::<i64>().ok().map(|_| raw.to_owned()),
    };
    resolved.or_else(|| raw.eq_ignore_ascii_case("unset").then(String::new))
}

impl AppState {
    pub(super) fn selected_model(&self) -> Option<&Model> {
        self.find_model(
            self.selected_provider_id.as_ref()?,
            self.selected_model_id.as_ref()?,
        )
    }

    /// The values a model would run with: the active ones for the model in use, otherwise the
    /// ones last set for it. A model that was never set up has none.
    pub(super) fn model_option_values(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> ModelOptionValues {
        let mut values = if self.is_current_model(provider_id, model_id) {
            self.selected_model_options.clone()
        } else {
            self.remembered_model_options
                .get(provider_id)
                .and_then(|models| models.get(model_id))
                .cloned()
                .unwrap_or_default()
        };
        if let Some(model) = self.find_model(provider_id, model_id) {
            kraai_types::reconcile_model_option_values(&model.options, &mut values);
        }
        values
    }

    /// Whether the settings sheet of a model holds a setting the model list cannot change.
    pub(super) fn model_has_more_settings(&self, provider_id: &str, model_id: &str) -> bool {
        self.find_model(provider_id, model_id).is_some_and(|model| {
            let values = self.model_option_values(provider_id, model_id);
            let inline = inline_option(model, &values).map(|option| option.id.as_str());
            active_options(model, &values).any(|option| Some(option.id.as_str()) != inline)
        })
    }

    pub(super) fn model_settings_view(&self) -> Option<(&Model, ModelOptionValues)> {
        let target = self.model_settings.as_ref()?;
        let model = self.find_model(&target.provider_id, &target.model_id)?;
        let values = self.model_option_values(&target.provider_id, &target.model_id);
        Some((model, values))
    }

    pub(super) fn highlighted_model_setting(&self) -> Option<String> {
        let (model, values) = self.model_settings_view()?;
        active_options(model, &values)
            .nth(self.sheet_index)
            .map(|option| option.id.clone())
    }

    /// Moves the highlight to a setting, or keeps it in range when the setting is gone.
    pub(super) fn highlight_model_setting(&mut self, id: &str) {
        let Some((model, values)) = self.model_settings_view() else {
            return;
        };
        let last = active_options(model, &values).count().saturating_sub(1);
        let index = active_options(model, &values)
            .position(|option| option.id == id)
            .unwrap_or(self.sheet_index);
        self.sheet_index = index.min(last);
    }
}

impl App {
    pub(super) fn reconcile_model_options(&mut self) {
        if let Some(model) = self.state.selected_model() {
            let mut values = self.state.selected_model_options.clone();
            kraai_types::reconcile_model_option_values(&model.options, &mut values);
            self.state.selected_model_options = values;
        }
    }

    /// Records the active values as the ones last used with the model in use.
    pub(super) fn remember_model_options(&mut self) {
        if let (Some(provider_id), Some(model_id)) = (
            self.state.selected_provider_id.clone(),
            self.state.selected_model_id.clone(),
        ) {
            self.state
                .remembered_model_options
                .entry(provider_id)
                .or_default()
                .insert(model_id, self.state.selected_model_options.clone());
        }
    }

    pub(super) fn set_model_option(
        &mut self,
        provider_id: &str,
        model_id: &str,
        id: &str,
        input: &str,
    ) -> Result<(), String> {
        let model = self
            .state
            .find_model(provider_id, model_id)
            .ok_or_else(|| String::from("This model is no longer available"))?;
        let mut values = kraai_types::parse_model_option_assignments(
            &model.options,
            &[format!("{id}={input}")],
            self.state.model_option_values(provider_id, model_id),
        )?;
        kraai_types::reconcile_model_option_values(&model.options, &mut values);
        self.state.status = if input.is_empty() {
            format!("Unset {id}")
        } else {
            format!("Selected {id}: {input}")
        };
        if self.state.is_current_model(provider_id, model_id) {
            self.state.selected_model_options = values;
            self.save_model_selection();
        } else {
            self.state
                .remembered_model_options
                .entry(provider_id.to_owned())
                .or_default()
                .insert(model_id.to_owned(), values);
            self.save_workspace_preferences();
        }
        Ok(())
    }

    /// Steps the reasoning effort of a model from the model list.
    pub(super) fn step_inline_option(&mut self, provider_id: &str, model_id: &str, forward: bool) {
        let values = self.state.model_option_values(provider_id, model_id);
        let Some((id, value)) = self
            .state
            .find_model(provider_id, model_id)
            .and_then(|model| inline_option(model, &values))
            .and_then(|option| {
                let value = stepped_choice(option, values.get(&option.id), forward)?;
                Some((option.id.clone(), value))
            })
        else {
            return;
        };
        if let Err(error) = self.set_model_option(provider_id, model_id, &id, &value) {
            self.state.sheet_notice = Some(error);
        }
    }

    pub(super) fn open_model_settings(&mut self, provider_id: String, model_id: String) {
        self.state.model_settings = Some(ModelSettingsTarget {
            provider_id,
            model_id,
        });
        self.state.sheet_query.clear();
        self.state.sheet_notice = None;
        self.state.mode = UiMode::ModelSettings;
        self.state.reset_sheet_selection();
    }

    /// Returns from the settings sheet to the model list.
    pub(super) fn close_model_settings(&mut self) {
        match self.state.model_settings.take() {
            Some(target) => self.return_to_model_sheet(&target.provider_id, &target.model_id),
            None => self.close_sheet(),
        }
    }

    pub(super) fn handle_model_settings_key_event(&mut self, key: KeyEvent) {
        self.state.sheet_notice = None;
        let Some(target) = self.state.model_settings.clone() else {
            self.close_sheet();
            return;
        };
        let query_is_empty = self.state.sheet_query.is_empty();
        if key.code == KeyCode::Backspace && key.modifiers.is_empty() && query_is_empty {
            self.close_model_settings();
            return;
        }
        let Some((model, values)) = self.state.model_settings_view() else {
            if key.code == KeyCode::Enter {
                self.close_model_settings();
            }
            return;
        };
        let active: Vec<_> = active_options(model, &values).cloned().collect();
        if self.edit_sheet_query(key) {
            return;
        }
        let len = active.len();
        let index = self.state.sheet_index.min(len.saturating_sub(1));
        let option = active.get(index);
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                self.state.sheet_index = model_menu_previous_index(index, len)
            }
            KeyCode::Down | KeyCode::Tab => {
                self.state.sheet_index = model_menu_next_index(index, len)
            }
            KeyCode::Left | KeyCode::Right => {
                let Some(option) = option else {
                    return;
                };
                let forward = key.code == KeyCode::Right;
                if let Some(value) = stepped_choice(option, values.get(&option.id), forward) {
                    self.apply_model_setting(&target, &option.id, &value);
                }
            }
            KeyCode::Delete if query_is_empty => {
                if let Some(option) = option {
                    self.apply_model_setting(&target, &option.id, "");
                }
            }
            KeyCode::Enter if self.state.sheet_query.trim().is_empty() => {
                self.select_model(&target.provider_id, &target.model_id)
            }
            KeyCode::Enter => {
                let Some(option) = option else {
                    self.state.sheet_notice = Some(String::from("No setting to change"));
                    return;
                };
                let query = self.state.sheet_query.trim().to_owned();
                let Some(value) = resolve_option_input(option, &query) else {
                    self.state.sheet_notice = Some(match option.kind {
                        ModelOptionKind::Integer { .. } => {
                            format!("{} takes a whole number", option.label)
                        }
                        ModelOptionKind::Choice { .. } | ModelOptionKind::Boolean { .. } => {
                            format!("{} has no choice named {query}", option.label)
                        }
                    });
                    return;
                };
                if self.apply_model_setting(&target, &option.id, &value) {
                    self.state.sheet_query.clear();
                }
            }
            _ => {}
        }
    }

    /// Sets a value from the settings sheet and keeps the highlight on that setting.
    fn apply_model_setting(&mut self, target: &ModelSettingsTarget, id: &str, value: &str) -> bool {
        if let Err(error) = self.set_model_option(&target.provider_id, &target.model_id, id, value)
        {
            self.state.sheet_notice = Some(error);
            return false;
        }
        self.state.highlight_model_setting(id);
        true
    }
}
