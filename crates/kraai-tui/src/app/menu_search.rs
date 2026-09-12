use super::*;

impl AppState {
    pub(super) fn reset_menu_selection(&mut self) {
        self.model_menu_index = 0;
        self.sessions_menu_index =
            usize::from(!self.menu_search.is_empty() && !self.filtered_sessions().is_empty());
    }

    pub(super) fn filtered_models(&self) -> Vec<(String, Model)> {
        let query = self.menu_search.to_lowercase();
        flatten_models_map(&self.models_by_provider)
            .into_iter()
            .filter(|(provider, model)| {
                format!("{provider} {} {}", model.id, model.name)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }

    pub(super) fn filtered_sessions(&self) -> Vec<&kraai_runtime::Session> {
        let query = self.menu_search.to_lowercase();
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
}

impl App {
    pub(super) fn handle_menu_search(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.state.menu_search.push(ch);
            }
            KeyCode::Backspace => {
                self.state.menu_search.pop();
            }
            _ => return false,
        }
        self.state.reset_menu_selection();
        true
    }
}
