use super::line_edit::LineKey;
use super::*;

impl AppState {
    pub(super) fn reset_sheet_selection(&mut self) {
        self.sheet_index = 0;
        self.sheet_scroll.set(0);
    }

    pub(super) fn wants_back(&self, key: KeyEvent) -> bool {
        key.code == KeyCode::Backspace && key.modifiers.is_empty() && self.sheet_query.is_empty()
    }

    pub(super) fn filtered_models(&self) -> Vec<(&str, &Model)> {
        let query = self.sheet_query.as_str().to_lowercase();
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

    pub(super) fn composer(&self) -> TextInput<'_> {
        match self.sheet_prefix() {
            Some(prefix) => TextInput::with_prefix(
                &prefix,
                self.sheet_query.as_str(),
                self.sheet_query.cursor(),
            ),
            None => TextInput::new(&self.input, self.input_cursor),
        }
    }
}

impl App {
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

    pub(super) fn leave_sheet(&mut self) {
        if self.state.mode != UiMode::Chat {
            self.close_sheet();
        }
    }

    pub(super) fn close_sheet_to_composer(&mut self, command: &str) {
        self.close_sheet();
        if self.state.input.is_empty() && self.state.draft_images.len() == 0 {
            self.set_input_text(command.to_owned());
            self.state.command_popup_dismissed = false;
        }
    }

    pub(super) fn filter_sheet(&mut self, key: KeyEvent, command: &str) -> bool {
        if self.state.wants_back(key) {
            self.close_sheet_to_composer(command);
            return true;
        }
        self.edit_sheet_filter(key)
    }

    pub(super) fn edit_sheet_filter(&mut self, key: KeyEvent) -> bool {
        let handled = self.state.sheet_query.handle_key(key);
        if handled == Some(LineKey::Edited) {
            self.state.reset_sheet_selection();
        }
        handled.is_some()
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "only Up, Down, Tab and BackTab walk a sheet, and other keys are left to the caller"
    )]
    pub(super) fn walk_sheet(&mut self, key: KeyEvent, len: usize) -> bool {
        let index = self.state.sheet_index.min(len.saturating_sub(1));
        self.state.sheet_index = match key.code {
            KeyCode::Up | KeyCode::BackTab => previous_wrapping(index, len),
            KeyCode::Down | KeyCode::Tab => next_wrapping(index, len),
            _ => return false,
        };
        true
    }

    pub(super) fn edit_sheet_query(&mut self, key: KeyEvent) -> bool {
        self.state.sheet_query.handle_key(key).is_some()
    }

    pub(super) fn paste_into_sheet_query(&mut self, text: &str) {
        self.state.sheet_query.insert(text);
    }
}
