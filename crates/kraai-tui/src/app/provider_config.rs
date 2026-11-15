use super::providers_sheet::ProviderAction;
use super::*;

/// One row of a provider's configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ConfigRow {
    ProviderId,
    ProviderType,
    ProviderValue(String),
    AddModel,
    /// The id of the model at this place in the settings.
    ModelId(usize),
    ModelValue(usize, String),
}

impl ConfigRow {
    /// The model the row belongs to, by its place in the settings.
    pub(super) fn model(&self) -> Option<usize> {
        match self {
            Self::ModelId(model) | Self::ModelValue(model, _) => Some(*model),
            Self::ProviderId | Self::ProviderType | Self::ProviderValue(_) | Self::AddModel => None,
        }
    }
}

/// How a row is changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConfigKind {
    /// Typed in the composer.
    Text,
    Toggle,
    /// The provider type, stepped through the kinds of provider Kraai knows.
    Type,
    AddModel,
}

/// A secret as the form shows it: dots, then the last four characters of a long one.
pub(super) fn masked(secret: &str) -> String {
    let characters = secret.chars().count();
    let tail: String = if characters >= 12 {
        secret.chars().skip(characters - 4).collect()
    } else {
        String::new()
    };
    format!("{}{tail}", "•".repeat(8))
}

impl AppState {
    /// The rows of the open provider's configuration, top to bottom.
    pub(super) fn config_rows(&self) -> Vec<ConfigRow> {
        let (Some(provider), Some(draft)) = (self.open_provider(), self.settings_draft.as_ref())
        else {
            return Vec::new();
        };
        let definition = self.provider_definition(&provider.type_id);
        let mut rows = vec![ConfigRow::ProviderId, ConfigRow::ProviderType];
        rows.extend(
            definition
                .iter()
                .flat_map(|definition| &definition.provider_fields)
                .map(|field| ConfigRow::ProviderValue(field.key.clone())),
        );
        rows.push(ConfigRow::AddModel);
        for (index, model) in draft.models.iter().enumerate() {
            if model.provider_id != provider.id {
                continue;
            }
            rows.push(ConfigRow::ModelId(index));
            rows.extend(
                definition
                    .iter()
                    .flat_map(|definition| &definition.model_fields)
                    .map(|field| ConfigRow::ModelValue(index, field.key.clone())),
            );
        }
        rows
    }

    pub(super) fn highlighted_config_row(&self) -> Option<ConfigRow> {
        let mut rows = self.config_rows();
        let index = self.sheet_index.min(rows.len().checked_sub(1)?);
        Some(rows.swap_remove(index))
    }

    /// The field a row sets. Ids, the type and the add row have none.
    pub(super) fn config_field(&self, row: &ConfigRow) -> Option<&FieldDefinition> {
        let definition = self.provider_definition(&self.open_provider()?.type_id)?;
        let (fields, key) = match row {
            ConfigRow::ProviderValue(key) => (&definition.provider_fields, key),
            ConfigRow::ModelValue(_, key) => (&definition.model_fields, key),
            ConfigRow::ProviderId
            | ConfigRow::ProviderType
            | ConfigRow::AddModel
            | ConfigRow::ModelId(_) => return None,
        };
        fields.iter().find(|field| &field.key == key)
    }

    pub(super) fn config_kind(&self, row: &ConfigRow) -> ConfigKind {
        match row {
            ConfigRow::ProviderType => ConfigKind::Type,
            ConfigRow::AddModel => ConfigKind::AddModel,
            _ if self.config_field(row).is_some_and(is_boolean_field) => ConfigKind::Toggle,
            _ => ConfigKind::Text,
        }
    }

    pub(super) fn config_label(&self, row: &ConfigRow) -> String {
        match row {
            ConfigRow::ProviderId | ConfigRow::ModelId(_) => String::from("ID"),
            ConfigRow::ProviderType => String::from("Type"),
            ConfigRow::AddModel => String::from("Add a model"),
            ConfigRow::ProviderValue(key) | ConfigRow::ModelValue(_, key) => self
                .config_field(row)
                .map_or_else(|| key.clone(), |field| field.label.clone()),
        }
    }

    /// What a row holds, as text. Empty when nothing is set.
    pub(super) fn config_value(&self, row: &ConfigRow) -> String {
        let model = |index: &usize| self.settings_draft.as_ref()?.models.get(*index);
        match row {
            ConfigRow::ProviderId => self.open_provider().map(|provider| provider.id.clone()),
            ConfigRow::ProviderType => self
                .open_provider()
                .map(|provider| provider.type_id.clone()),
            ConfigRow::ProviderValue(key) => self
                .open_provider()
                .map(|provider| field_value_display(&provider.values, key)),
            ConfigRow::ModelId(index) => model(index).map(|model| model.id.clone()),
            ConfigRow::ModelValue(index, key) => {
                model(index).map(|model| field_value_display(&model.values, key))
            }
            ConfigRow::AddModel => None,
        }
        .unwrap_or_default()
    }

    /// The row holds something to keep off the screen, such as an API key.
    pub(super) fn config_secret(&self, row: &ConfigRow) -> bool {
        self.config_field(row).is_some_and(|field| field.secret)
    }

    /// Saving the edit as typed would erase a secret the row holds.
    pub(super) fn edit_clears_secret(&self, row: &ConfigRow) -> bool {
        self.config_secret(row)
            && self.sheet_query.trim().is_empty()
            && !self.config_value(row).is_empty()
    }

    /// Why the last save refused what a row holds.
    pub(super) fn config_error(&self, row: &ConfigRow) -> Option<&str> {
        let provider = self.settings_provider_index;
        let key = match row {
            ConfigRow::ProviderId => format!("providers[{provider}].id"),
            ConfigRow::ProviderType => format!("providers[{provider}].type_id"),
            ConfigRow::ProviderValue(key) => format!("providers[{provider}].{key}"),
            ConfigRow::ModelId(model) => format!("models[{model}].id"),
            ConfigRow::ModelValue(model, key) => format!("models[{model}].{key}"),
            ConfigRow::AddModel => return None,
        };
        self.settings_errors.get(&key).map(String::as_str)
    }
}

impl App {
    pub(super) fn handle_provider_config_key(&mut self, key: KeyEvent) {
        let armed = std::mem::take(&mut self.state.sheet_armed);
        if let Some(row) = self.state.config_editing.clone() {
            if key.code != KeyCode::Enter {
                self.edit_sheet_query(key);
            } else if self.state.edit_clears_secret(&row) && !armed {
                self.state.sheet_armed = true;
            } else {
                self.commit_config_edit(&row);
            }
            return;
        }
        if self.state.wants_back(key) {
            self.return_to_provider(ProviderAction::EditConfig);
            return;
        }
        let rows = self.state.config_rows().len();
        if self.walk_sheet(key, rows) {
            return;
        }
        let Some(row) = self.state.highlighted_config_row() else {
            return;
        };
        let typed = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match (key.code, self.state.config_kind(&row)) {
            (KeyCode::Delete, _) => match row.model() {
                Some(model) if armed => self.remove_config_model(model),
                Some(_) => self.state.sheet_armed = true,
                None => {}
            },
            (KeyCode::Enter, ConfigKind::Text) => {
                self.state.sheet_query = if self.state.config_secret(&row) {
                    String::new()
                } else {
                    self.state.config_value(&row)
                };
                self.state.config_editing = Some(row);
            }
            (KeyCode::Char(ch), ConfigKind::Text) if typed => {
                self.state.sheet_query = ch.to_string();
                self.state.config_editing = Some(row);
            }
            (
                KeyCode::Enter | KeyCode::Left | KeyCode::Right | KeyCode::Char(' '),
                ConfigKind::Toggle,
            ) => self.toggle_config_field(&row),
            (KeyCode::Enter | KeyCode::Right, ConfigKind::Type) => self.cycle_provider_type(true),
            (KeyCode::Left, ConfigKind::Type) => self.cycle_provider_type(false),
            (KeyCode::Enter, ConfigKind::AddModel) => self.add_config_model(),
            _ => {}
        }
    }

    /// Starts typing a value for the highlighted row from pasted text.
    pub(super) fn paste_into_provider_config(&mut self, text: &str) {
        if self.state.config_editing.is_none() {
            let Some(row) = self
                .state
                .highlighted_config_row()
                .filter(|row| self.state.config_kind(row) == ConfigKind::Text)
            else {
                return;
            };
            self.state.sheet_query.clear();
            self.state.config_editing = Some(row);
        }
        self.paste_into_sheet_query(text);
    }

    /// The typed text as the value of a field, or why the field cannot take it. No text clears
    /// the field.
    fn typed_field_value(
        field: &FieldDefinition,
        text: &str,
    ) -> Result<Option<SettingsValue>, String> {
        if text.is_empty() {
            return Ok(None);
        }
        parse_field_input(field, text)
            .map(Some)
            .ok_or_else(|| match field.value_kind {
                kraai_runtime::FieldValueKind::Integer => {
                    format!("{} takes a whole number", field.label)
                }
                _ => format!("{} cannot be {text}", field.label),
            })
    }

    fn commit_config_edit(&mut self, row: &ConfigRow) {
        let text = self.state.sheet_query.trim().to_owned();
        let value = match self.state.config_field(row) {
            Some(field) => match Self::typed_field_value(field, &text) {
                Ok(value) => value,
                Err(reason) => {
                    self.state.sheet_notice = Some(reason);
                    return;
                }
            },
            None => None,
        };
        let provider_index = self.state.settings_provider_index;
        let Some(draft) = self.state.settings_draft.as_mut() else {
            return;
        };
        let store = |values: &mut Vec<kraai_runtime::FieldValueEntry>, key: &str| match value {
            Some(value) => set_field_value(values, key, value),
            None => clear_field_value(values, key),
        };
        match row {
            ConfigRow::ProviderId => {
                if draft
                    .providers
                    .iter()
                    .enumerate()
                    .any(|(index, provider)| index != provider_index && provider.id == text)
                {
                    self.state.sheet_notice = Some(format!("A provider is already named {text}"));
                    return;
                }
                let Some(provider) = draft.providers.get_mut(provider_index) else {
                    return;
                };
                let previous = std::mem::replace(&mut provider.id, text.clone());
                for model in &mut draft.models {
                    if model.provider_id == previous {
                        model.provider_id.clone_from(&text);
                    }
                }
            }
            ConfigRow::ProviderValue(key) => {
                if let Some(provider) = draft.providers.get_mut(provider_index) {
                    store(&mut provider.values, key);
                }
            }
            ConfigRow::ModelId(model) => {
                if let Some(model) = draft.models.get_mut(*model) {
                    model.id = text;
                }
            }
            ConfigRow::ModelValue(model, key) => {
                if let Some(model) = draft.models.get_mut(*model) {
                    store(&mut model.values, key);
                }
            }
            ConfigRow::ProviderType | ConfigRow::AddModel => {}
        }
        self.state.config_editing = None;
        self.state.sheet_query.clear();
        self.save_settings_draft();
    }

    fn toggle_config_field(&mut self, row: &ConfigRow) {
        let enabled = self.state.config_value(row) == "yes";
        let provider_index = self.state.settings_provider_index;
        let Some(draft) = self.state.settings_draft.as_mut() else {
            return;
        };
        let values = match row {
            ConfigRow::ProviderValue(_) => draft
                .providers
                .get_mut(provider_index)
                .map(|provider| &mut provider.values),
            ConfigRow::ModelValue(model, _) => {
                draft.models.get_mut(*model).map(|model| &mut model.values)
            }
            _ => None,
        };
        if let (Some(values), ConfigRow::ProviderValue(key) | ConfigRow::ModelValue(_, key)) =
            (values, row)
        {
            set_field_value(values, key, SettingsValue::Bool(!enabled));
            self.save_settings_draft();
        }
    }

    fn cycle_provider_type(&mut self, forward: bool) {
        let provider_index = self.state.settings_provider_index;
        let len = self.state.provider_definitions.len();
        let Some(provider) = self
            .state
            .settings_draft
            .as_mut()
            .and_then(|draft| draft.providers.get_mut(provider_index))
        else {
            return;
        };
        let current = self
            .state
            .provider_definitions
            .iter()
            .position(|definition| definition.type_id == provider.type_id)
            .unwrap_or(0);
        let next = if forward {
            next_wrapping(current, len)
        } else {
            previous_wrapping(current, len)
        };
        if let Some(definition) = self.state.provider_definitions.get(next) {
            provider.type_id.clone_from(&definition.type_id);
            provider.values = merge_values(&definition.provider_fields, &provider.values);
            self.save_settings_draft();
        }
    }

    fn add_config_model(&mut self) {
        let Some((provider_id, fields)) = self.state.open_provider().map(|provider| {
            (
                provider.id.clone(),
                self.state
                    .provider_definition(&provider.type_id)
                    .map(|definition| definition.model_fields.clone())
                    .unwrap_or_default(),
            )
        }) else {
            return;
        };
        let Some(draft) = self.state.settings_draft.as_mut() else {
            return;
        };
        let Some(id) = (1..).map(|number| format!("model-{number}")).find(|id| {
            !draft
                .models
                .iter()
                .any(|model| model.provider_id == provider_id && model.id == *id)
        }) else {
            return;
        };
        draft.models.push(ModelSettings {
            options: Vec::new(),
            remove_options: Vec::new(),
            id,
            provider_id,
            values: default_values(&fields),
        });
        let added = ConfigRow::ModelId(draft.models.len() - 1);
        if let Some(index) = self
            .state
            .config_rows()
            .iter()
            .position(|row| *row == added)
        {
            self.state.sheet_index = index;
        }
        self.save_settings_draft();
    }

    fn remove_config_model(&mut self, model: usize) {
        let Some(draft) = self.state.settings_draft.as_mut() else {
            return;
        };
        if model >= draft.models.len() {
            return;
        }
        let removed = draft.models.remove(model);
        self.provider_feedback(format!("Removed model {}", removed.id));
        self.save_settings_draft();
    }
}
