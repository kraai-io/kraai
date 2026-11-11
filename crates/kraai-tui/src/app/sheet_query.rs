use super::*;

impl AppState {
    pub(super) fn reset_sheet_selection(&mut self) {
        self.sheet_index = 0;
        self.sheet_scroll.set(0);
        self.sessions_menu_index =
            usize::from(!self.sheet_query.is_empty() && !self.filtered_sessions().is_empty());
    }

    pub(super) fn filtered_models(&self) -> Vec<(&str, &Model)> {
        let query = self.sheet_query.to_lowercase();
        let mut providers: Vec<_> = self.models_by_provider.iter().collect();
        providers.sort_by_key(|(provider, _)| *provider);
        providers
            .into_iter()
            .flat_map(|(provider, models)| {
                models.iter().map(move |model| (provider.as_str(), model))
            })
            .filter(|(provider, model)| {
                query.is_empty()
                    || format!("{provider} {} {}", model.id, model.name)
                        .to_lowercase()
                        .contains(&query)
            })
            .collect()
    }

    pub(super) fn filtered_sessions(&self) -> Vec<&kraai_runtime::Session> {
        let query = self.sheet_query.to_lowercase();
        self.sessions
            .iter()
            .filter(|session| {
                format!(
                    "{} {} {}",
                    session.id,
                    session.title.as_deref().unwrap_or_default(),
                    session.workspace_dir
                )
                .to_lowercase()
                .contains(&query)
            })
            .collect()
    }

    /// The breadcrumb shown ahead of the query while a sheet owns the composer.
    pub(super) fn sheet_prefix(&self) -> Option<String> {
        match self.mode {
            UiMode::Model => Some(String::from("/model ")),
            UiMode::ModelSettings => self.model_settings.as_ref().map(|target| {
                format!(
                    "/model › {} › ",
                    self.find_model(&target.provider_id, &target.model_id)
                        .map_or(target.model_id.as_str(), |model| model.name.as_str())
                )
            }),
            _ => None,
        }
    }

    pub(super) fn composer(&self) -> TextInput<'_> {
        match self.sheet_prefix() {
            Some(prefix) => {
                TextInput::with_prefix(&prefix, &self.sheet_query, self.sheet_query.len())
            }
            None => TextInput::new(&self.input, self.input_cursor),
        }
    }
}

impl App {
    /// Applies a typing key to the sheet query; false when the key is not an edit.
    pub(super) fn edit_sheet_query(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let query = &mut self.state.sheet_query;
        match key.code {
            KeyCode::Backspace if control || alt => {
                query.truncate(composer::word_left(query, query.len()))
            }
            KeyCode::Char('u') if control => query.clear(),
            KeyCode::Backspace => {
                query.pop();
            }
            KeyCode::Char(ch) if !control && !alt => query.push(ch),
            _ => return false,
        }
        true
    }

    pub(super) fn paste_into_sheet_query(&mut self, text: &str) {
        self.state
            .sheet_query
            .extend(text.chars().filter(|ch| !ch.is_control()));
    }
}
