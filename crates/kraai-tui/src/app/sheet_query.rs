use super::line_edit::LineKey;
use super::types::{ProvidersView, UiMode};
use super::ui::{next_wrapping, previous_wrapping};
use super::{App, AppState};
use crate::components::TextInput;
use kraai_runtime::Model;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

impl AppState {
    pub(super) fn wants_back(&self, key: KeyEvent) -> bool {
        key.code == KeyCode::Backspace && key.modifiers.is_empty() && self.sheet.query.is_empty()
    }

    pub(super) fn filtered_models(&self) -> Vec<(&str, &Model)> {
        let query = self.sheet.query.as_str().to_lowercase();
        let mut providers: Vec<_> = self.models.by_provider.iter().collect();
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
            UiMode::Mcp => Some(match &self.mcp.open {
                Some(server) => format!("/mcp › {server} › "),
                None => String::from("/mcp "),
            }),
            UiMode::ModelSettings => self.sheet.model_settings.as_ref().map(|target| {
                format!(
                    "/model › {} › ",
                    self.find_model(&target.provider_id, &target.model_id)
                        .map_or(target.model_id.as_str(), |model| model.name.as_str())
                )
            }),
            UiMode::Chat => None,
        }
    }

    pub(super) fn composer_input(&self) -> TextInput<'_> {
        match self.sheet_prefix() {
            Some(prefix) => TextInput::with_prefix(
                &prefix,
                self.sheet.query.as_str(),
                self.sheet.query.cursor(),
            ),
            None => TextInput::new(&self.composer.text, self.composer.cursor),
        }
    }
}

impl App {
    pub(super) fn open_sheet(&mut self, mode: UiMode) {
        self.state.sheet.query.clear();
        self.state.sheet.notice = None;
        self.state.sheet.feedback = None;
        self.state.sheet.armed = false;
        self.state.sheet.model_settings = None;
        self.state.providers.config_editing = None;
        self.state.mode = mode;
        self.state.sheet.reset_selection();
    }

    pub(super) fn close_sheet(&mut self) {
        self.open_sheet(UiMode::Chat);
        self.state.mcp.open = None;
        self.state.providers.view = ProvidersView::List;
        self.reset_command_selection();
    }

    pub(super) fn leave_sheet(&mut self) {
        if self.state.mode != UiMode::Chat {
            self.close_sheet();
        }
    }

    pub(super) fn close_sheet_to_composer(&mut self, command: &str) {
        self.close_sheet();
        if self.state.composer.text.is_empty() && self.state.composer.images.len() == 0 {
            self.set_input_text(command.to_owned());
            self.state.composer.command_popup_dismissed = false;
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
        let handled = self.state.sheet.query.handle_key(key);
        if handled == Some(LineKey::Edited) {
            self.state.sheet.reset_selection();
        }
        handled.is_some()
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "only Up, Down, Tab and BackTab walk a sheet, and other keys are left to the caller"
    )]
    pub(super) fn walk_sheet(&mut self, key: KeyEvent, len: usize) -> bool {
        let index = self.state.sheet.index.min(len.saturating_sub(1));
        self.state.sheet.index = match key.code {
            KeyCode::Up | KeyCode::BackTab => previous_wrapping(index, len),
            KeyCode::Down | KeyCode::Tab => next_wrapping(index, len),
            _ => return false,
        };
        true
    }

    pub(super) fn edit_sheet_query(&mut self, key: KeyEvent) -> bool {
        self.state.sheet.query.handle_key(key).is_some()
    }

    pub(super) fn paste_into_sheet_query(&mut self, text: &str) {
        self.state.sheet.query.insert(text);
    }
}
