use super::*;

impl AppState {
    pub(super) fn reset_sheet_selection(&mut self) {
        self.sheet_index = 0;
        self.sheet_scroll.set(0);
    }

    /// Backspace with nothing typed asks to leave the sheet the way it was entered.
    pub(super) fn wants_back(&self, key: KeyEvent) -> bool {
        key.code == KeyCode::Backspace && key.modifiers.is_empty() && self.sheet_query.is_empty()
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

    /// The breadcrumb shown ahead of the query while a sheet owns the composer.
    pub(super) fn sheet_prefix(&self) -> Option<String> {
        match self.mode {
            UiMode::Model => Some(String::from("/model ")),
            UiMode::Agent => Some(String::from("/agent ")),
            UiMode::Sessions => Some(String::from("/sessions ")),
            UiMode::Providers => Some(self.providers_prefix()),
            UiMode::Mcp => Some(match &self.mcp_open {
                Some(server) => format!("/mcp › {server} › "),
                None => String::from("/mcp "),
            }),
            UiMode::ModelSettings => self.model_settings.as_ref().map(|target| {
                format!(
                    "/model › {} › ",
                    self.find_model(&target.provider_id, &target.model_id)
                        .map_or(target.model_id.as_str(), |model| model.name.as_str())
                )
            }),
            UiMode::Chat => None,
        }
    }

    /// The sheet query as the composer shows it. A secret being typed shows as dots.
    fn typed_in_sheet(&self) -> std::borrow::Cow<'_, str> {
        let secret = self
            .config_editing
            .as_ref()
            .is_some_and(|row| self.config_secret(row));
        if secret {
            "•".repeat(self.sheet_query.chars().count()).into()
        } else {
            self.sheet_query.as_str().into()
        }
    }

    pub(super) fn composer(&self) -> TextInput<'_> {
        match self.sheet_prefix() {
            Some(prefix) => {
                let typed = self.typed_in_sheet();
                TextInput::with_prefix(&prefix, &typed, typed.len())
            }
            None => TextInput::new(&self.input, self.input_cursor),
        }
    }
}

impl App {
    /// Opens a sheet with nothing typed and its first row highlighted.
    pub(super) fn open_sheet(&mut self, mode: UiMode) {
        self.state.sheet_query.clear();
        self.state.sheet_notice = None;
        self.state.sheet_feedback = None;
        self.state.sheet_armed = false;
        self.state.model_settings = None;
        self.state.config_editing = None;
        self.state.mode = mode;
        self.state.reset_sheet_selection();
    }

    pub(super) fn close_sheet(&mut self) {
        self.open_sheet(UiMode::Chat);
        self.state.mcp_open = None;
        self.state.providers_view = ProvidersView::List;
        self.reset_command_selection();
    }

    /// Returns to the chat from whatever sheet is open, leaving the command list alone.
    pub(super) fn leave_sheet(&mut self) {
        if self.state.mode != UiMode::Chat {
            self.close_sheet();
        }
    }

    /// Closes the sheet and puts `command` back in an empty composer, as if backspacing into it.
    pub(super) fn close_sheet_to_composer(&mut self, command: &str) {
        self.close_sheet();
        if self.state.input.is_empty() && self.state.draft_images.len() == 0 {
            self.set_input_text(command.to_owned());
            self.state.command_popup_dismissed = false;
        }
    }

    /// Applies a key that edits the filter of a sheet a command opened. Backspace on an empty
    /// filter hands `command` back to the composer. False when the key is not an edit.
    pub(super) fn filter_sheet(&mut self, key: KeyEvent, command: &str) -> bool {
        if self.state.wants_back(key) {
            self.close_sheet_to_composer(command);
            return true;
        }
        self.edit_sheet_filter(key)
    }

    /// Applies a key that edits a sheet's filter and puts the highlight back on the first row.
    /// False when the key is not an edit.
    pub(super) fn edit_sheet_filter(&mut self, key: KeyEvent) -> bool {
        let edited = self.edit_sheet_query(key);
        if edited {
            self.state.reset_sheet_selection();
        }
        edited
    }

    /// Moves the highlight through `len` rows with the arrows, Tab and Shift+Tab, wrapping at
    /// both ends. False when the key is something else.
    pub(super) fn walk_sheet(&mut self, key: KeyEvent, len: usize) -> bool {
        let index = self.state.sheet_index.min(len.saturating_sub(1));
        self.state.sheet_index = match key.code {
            KeyCode::Up | KeyCode::BackTab => previous_wrapping(index, len),
            KeyCode::Down | KeyCode::Tab => next_wrapping(index, len),
            _ => return false,
        };
        true
    }

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
