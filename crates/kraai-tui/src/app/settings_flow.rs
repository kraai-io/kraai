use super::*;

impl App {
    pub(super) fn move_settings_selection(&mut self, delta: isize) {
        self.state.settings_delete_armed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderList => {
                let len = self
                    .state
                    .settings_draft
                    .as_ref()
                    .map_or(0, |draft| draft.providers.len());
                self.state.settings_provider_index =
                    adjust_index(self.state.settings_provider_index, len, delta);
                self.state.settings_model_index = 0;
            }
            SettingsFocus::ProviderForm => {
                let len = self.current_provider_fields().len();
                self.state.settings_provider_field_index =
                    adjust_index(self.state.settings_provider_field_index, len, delta);
            }
            SettingsFocus::ModelList => {
                let len = self.current_model_indices().len();
                self.state.settings_model_index =
                    adjust_index(self.state.settings_model_index, len, delta);
            }
            SettingsFocus::ModelForm => {
                let len = self.current_model_fields().len();
                self.state.settings_model_field_index =
                    adjust_index(self.state.settings_model_field_index, len, delta);
            }
        }
    }

    pub(super) fn adjust_settings_field(&mut self, forward: bool) {
        match self.state.settings_focus {
            SettingsFocus::ProviderForm => {
                if self.current_provider_field() == Some(SettingsProviderField::TypeId) {
                    self.cycle_provider_type(forward);
                } else if self.current_provider_field().is_some_and(|field| {
                    matches!(field, SettingsProviderField::Value(ref key) if self.current_provider_field_definition(key).is_some_and(is_boolean_field))
                }) {
                    self.toggle_settings_field();
                }
            }
            SettingsFocus::ModelForm | SettingsFocus::ProviderList | SettingsFocus::ModelList => {}
        }
    }

    pub(super) fn activate_settings_field(&mut self) {
        self.state.settings_delete_armed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderForm => match self.current_provider_field() {
                Some(SettingsProviderField::TypeId) => self.cycle_provider_type(true),
                Some(SettingsProviderField::Value(ref key))
                    if self
                        .current_provider_field_definition(key)
                        .is_some_and(is_boolean_field) =>
                {
                    self.toggle_settings_field()
                }
                Some(field) => self.start_provider_editor(field),
                None => {}
            },
            SettingsFocus::ModelForm => {
                if let Some(field) = self.current_model_field() {
                    self.start_model_editor(field);
                }
            }
            SettingsFocus::ProviderList | SettingsFocus::ModelList => {}
        }
    }

    pub(super) fn toggle_settings_field(&mut self) {
        let provider_index = self.state.settings_provider_index;
        let Some(SettingsProviderField::Value(key)) = self.current_provider_field() else {
            return;
        };
        let mut changed = false;
        if let Some(provider) = self
            .state
            .settings_draft
            .as_mut()
            .and_then(|draft| draft.providers.get_mut(provider_index))
        {
            let current = provider
                .values
                .iter()
                .find(|value| value.key == key)
                .and_then(|value| match value.value {
                    SettingsValue::Bool(value) => Some(value),
                    SettingsValue::String(_) | SettingsValue::Integer(_) => None,
                })
                .unwrap_or(false);
            set_field_value(&mut provider.values, &key, SettingsValue::Bool(!current));
            changed = true;
        }
        if changed {
            self.save_settings_draft();
        }
    }

    pub(super) fn add_settings_item(&mut self) {
        self.state.settings_delete_armed = false;
        let mut changed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderList | SettingsFocus::ProviderForm => {
                if let Some(draft) = self.state.settings_draft.as_mut() {
                    let Some(definition) = self.state.provider_definitions.first() else {
                        self.state.status = String::from("No provider definitions registered");
                        return;
                    };
                    let next_index = draft.providers.len();
                    draft.providers.push(ProviderSettings {
                        id: if next_index == 0 {
                            definition.default_provider_id_prefix.clone()
                        } else {
                            format!(
                                "{}-{}",
                                definition.default_provider_id_prefix,
                                next_index + 1
                            )
                        },
                        type_id: definition.type_id.clone(),
                        values: default_values(&definition.provider_fields),
                    });
                    self.state.settings_provider_index = next_index;
                    self.state.settings_model_index = 0;
                    self.state.status = String::from("Added provider");
                    changed = true;
                }
            }
            SettingsFocus::ModelList | SettingsFocus::ModelForm => {
                let provider_id = self.current_provider().map(|provider| provider.id.clone());
                let model_fields = self
                    .current_provider_definition()
                    .map(|definition| definition.model_fields.clone())
                    .unwrap_or_default();
                if let (Some(draft), Some(provider_id)) =
                    (self.state.settings_draft.as_mut(), provider_id)
                {
                    let next_count = draft
                        .models
                        .iter()
                        .filter(|model| model.provider_id == provider_id)
                        .count();
                    draft.models.push(ModelSettings {
                        id: format!("model-{}", next_count + 1),
                        provider_id,
                        values: default_values(&model_fields),
                    });
                    self.state.settings_model_index = next_count;
                    self.state.status = String::from("Added model");
                    changed = true;
                }
            }
        }
        if changed {
            self.save_settings_draft();
        }
    }

    pub(super) fn delete_settings_item(&mut self) {
        if !self.state.settings_delete_armed {
            self.state.settings_delete_armed = true;
            self.state.status = String::from("Press x again to confirm delete");
            return;
        }

        self.state.settings_delete_armed = false;
        let mut changed = false;
        match self.state.settings_focus {
            SettingsFocus::ProviderList | SettingsFocus::ProviderForm => {
                let provider_id = self.current_provider().map(|provider| provider.id.clone());
                if let (Some(draft), Some(provider_id)) =
                    (self.state.settings_draft.as_mut(), provider_id)
                {
                    draft
                        .providers
                        .retain(|provider| provider.id != provider_id);
                    draft
                        .models
                        .retain(|model| model.provider_id != provider_id);
                    self.state.settings_provider_index = self
                        .state
                        .settings_provider_index
                        .saturating_sub(1)
                        .min(draft.providers.len().saturating_sub(1));
                    self.state.settings_model_index = 0;
                    self.state.status = String::from("Deleted provider");
                    changed = true;
                }
            }
            SettingsFocus::ModelList | SettingsFocus::ModelForm => {
                let selected = self
                    .current_model()
                    .map(|model| (model.provider_id.clone(), model.id.clone()));
                if let (Some(draft), Some((provider_id, model_id))) =
                    (self.state.settings_draft.as_mut(), selected)
                {
                    draft.models.retain(|model| {
                        !(model.provider_id == provider_id && model.id == model_id)
                    });
                    self.state.settings_model_index =
                        self.state.settings_model_index.saturating_sub(1);
                    self.state.status = String::from("Deleted model");
                    changed = true;
                }
            }
        }
        if changed {
            self.save_settings_draft();
        }
    }

    pub(super) fn save_settings_draft(&mut self) {
        if let Some(settings) = self.state.settings_draft.clone() {
            self.request(RuntimeRequest::SaveSettings { settings });
        }
    }

    pub(super) fn start_provider_editor(&mut self, field: SettingsProviderField) {
        let Some(provider) = self.current_provider() else {
            return;
        };
        let value = match &field {
            SettingsProviderField::Id => provider.id.clone(),
            SettingsProviderField::TypeId => return,
            SettingsProviderField::Value(key) => {
                if self
                    .current_provider_field_definition(key)
                    .is_some_and(is_boolean_field)
                {
                    return;
                }
                field_value_display(&provider.values, key)
            }
        };
        self.state.settings_editor = Some(ActiveSettingsEditor::Provider(field));
        self.state.settings_editor_input = value;
    }

    pub(super) fn start_model_editor(&mut self, field: SettingsModelField) {
        let Some(model) = self.current_model() else {
            return;
        };
        let value = match &field {
            SettingsModelField::Id => model.id.clone(),
            SettingsModelField::Value(key) => field_value_display(&model.values, key),
        };
        self.state.settings_editor = Some(ActiveSettingsEditor::Model(field));
        self.state.settings_editor_input = value;
    }

    pub(super) fn commit_settings_editor(&mut self) {
        let Some(editor) = self.state.settings_editor.take() else {
            return;
        };
        let value = self.state.settings_editor_input.trim().to_string();
        let mut changed = false;

        match editor {
            ActiveSettingsEditor::Provider(field) => {
                let provider_index = self.state.settings_provider_index;
                let provider_field_definition = match &field {
                    SettingsProviderField::Value(key) => {
                        self.current_provider_field_definition(key).cloned()
                    }
                    SettingsProviderField::Id | SettingsProviderField::TypeId => None,
                };
                if let Some(draft) = self.state.settings_draft.as_mut()
                    && let Some(provider) = draft.providers.get_mut(provider_index)
                {
                    match field {
                        SettingsProviderField::Id => {
                            let previous_id = provider.id.clone();
                            provider.id = value.clone();
                            for model in &mut draft.models {
                                if model.provider_id == previous_id {
                                    model.provider_id = value.clone();
                                }
                            }
                            changed = true;
                        }
                        SettingsProviderField::TypeId => {}
                        SettingsProviderField::Value(key) => {
                            if let Some(definition) = provider_field_definition
                                && let Some(next_value) =
                                    parse_field_input(&definition, value.as_str())
                            {
                                set_field_value(&mut provider.values, &key, next_value);
                            } else {
                                clear_field_value(&mut provider.values, &key);
                            }
                            changed = true;
                        }
                    }
                }
            }
            ActiveSettingsEditor::Model(field) => {
                let model_field_definition = match &field {
                    SettingsModelField::Value(key) => {
                        self.current_model_field_definition(key).cloned()
                    }
                    SettingsModelField::Id => None,
                };
                if let Some(global_index) = self.current_model_global_index()
                    && let Some(model) = self
                        .state
                        .settings_draft
                        .as_mut()
                        .and_then(|draft| draft.models.get_mut(global_index))
                {
                    match field {
                        SettingsModelField::Id => {
                            model.id = value;
                            changed = true;
                        }
                        SettingsModelField::Value(key) => {
                            if let Some(definition) = model_field_definition
                                && let Some(next_value) =
                                    parse_field_input(&definition, value.as_str())
                            {
                                set_field_value(&mut model.values, &key, next_value);
                            } else {
                                clear_field_value(&mut model.values, &key);
                            }
                            changed = true;
                        }
                    }
                }
            }
        }

        self.state.settings_editor_input.clear();
        if changed {
            self.save_settings_draft();
        }
    }

    pub(super) fn cycle_provider_type(&mut self, forward: bool) {
        let provider_index = self.state.settings_provider_index;
        let Some(provider) = self
            .state
            .settings_draft
            .as_mut()
            .and_then(|draft| draft.providers.get_mut(provider_index))
        else {
            return;
        };
        if self.state.provider_definitions.is_empty() {
            return;
        }
        let len = self.state.provider_definitions.len();
        let current_index = self
            .state
            .provider_definitions
            .iter()
            .position(|definition| definition.type_id == provider.type_id)
            .unwrap_or(0);
        let next_index = if forward {
            (current_index + 1) % len
        } else {
            (current_index + len - 1) % len
        };
        if let Some(definition) = self.state.provider_definitions.get(next_index) {
            provider.type_id = definition.type_id.clone();
            provider.values = merge_values(&definition.provider_fields, &provider.values);
            self.save_settings_draft();
        }
    }

    pub(super) fn current_provider(&self) -> Option<&ProviderSettings> {
        self.state
            .settings_draft
            .as_ref()
            .and_then(|draft| draft.providers.get(self.state.settings_provider_index))
    }

    pub(super) fn current_provider_definition(&self) -> Option<&ProviderDefinition> {
        let provider = self.current_provider()?;
        self.state
            .provider_definitions
            .iter()
            .find(|definition| definition.type_id == provider.type_id)
    }

    pub(super) fn current_provider_fields(&self) -> Vec<SettingsProviderField> {
        let mut fields = vec![SettingsProviderField::Id, SettingsProviderField::TypeId];
        if let Some(definition) = self.current_provider_definition() {
            fields.extend(
                definition
                    .provider_fields
                    .iter()
                    .map(|field| SettingsProviderField::Value(field.key.clone())),
            );
        }
        fields
    }

    pub(super) fn current_provider_field(&self) -> Option<SettingsProviderField> {
        self.current_provider_fields()
            .get(self.state.settings_provider_field_index)
            .cloned()
    }

    pub(super) fn current_provider_field_definition(&self, key: &str) -> Option<&FieldDefinition> {
        self.current_provider_definition()?
            .provider_fields
            .iter()
            .find(|field| field.key == key)
    }

    pub(super) fn current_model_indices(&self) -> Vec<usize> {
        let Some(provider) = self.current_provider() else {
            return Vec::new();
        };
        self.state
            .settings_draft
            .as_ref()
            .map(|draft| {
                draft
                    .models
                    .iter()
                    .enumerate()
                    .filter_map(|(index, model)| {
                        (model.provider_id == provider.id).then_some(index)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn current_model_global_index(&self) -> Option<usize> {
        self.current_model_indices()
            .get(self.state.settings_model_index)
            .copied()
    }

    pub(super) fn current_model(&self) -> Option<&ModelSettings> {
        let index = self.current_model_global_index()?;
        self.state
            .settings_draft
            .as_ref()
            .and_then(|draft| draft.models.get(index))
    }

    pub(super) fn current_model_fields(&self) -> Vec<SettingsModelField> {
        let mut fields = vec![SettingsModelField::Id];
        if let Some(definition) = self.current_provider_definition() {
            fields.extend(
                definition
                    .model_fields
                    .iter()
                    .map(|field| SettingsModelField::Value(field.key.clone())),
            );
        }
        fields
    }

    pub(super) fn current_model_field_definition(&self, key: &str) -> Option<&FieldDefinition> {
        self.current_provider_definition()?
            .model_fields
            .iter()
            .find(|field| field.key == key)
    }

    pub(super) fn current_model_field(&self) -> Option<SettingsModelField> {
        self.current_model_fields()
            .get(self.state.settings_model_field_index)
            .cloned()
    }
}
