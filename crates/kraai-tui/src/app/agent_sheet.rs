use super::*;

impl AppState {
    /// The agents the agent list shows: every agent, or the ones the typed text names.
    pub(super) fn filtered_agents(&self) -> Vec<&kraai_runtime::AgentProfileSummary> {
        let query = self.sheet_query.to_lowercase();
        self.agent_profiles
            .iter()
            .filter(|profile| {
                query.is_empty()
                    || format!(
                        "{} {} {}",
                        profile.id, profile.display_name, profile.description
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }

    /// Puts the highlight of the unfiltered agent list on the agent in use.
    pub(super) fn highlight_selected_agent(&mut self) {
        if self.mode != UiMode::Agent || !self.sheet_query.is_empty() {
            return;
        }
        self.sheet_index = self
            .agent_profiles
            .iter()
            .position(|profile| Some(&profile.id) == self.selected_profile_id.as_ref())
            .unwrap_or(0);
    }
}

impl App {
    pub(super) fn open_agent_sheet(&mut self) {
        if self.state.turn_blocks_user_commands() {
            return;
        }
        self.open_sheet(UiMode::Agent);
        self.state.highlight_selected_agent();
        if let Some(session_id) = self.state.current_session_id.clone() {
            self.request(RuntimeRequest::GetSessionSnapshot { session_id });
        } else {
            self.request(RuntimeRequest::GetAgentProfileCatalog);
        }
    }

    pub(super) fn handle_agent_key_event(&mut self, key: KeyEvent) {
        let len = self.state.filtered_agents().len();
        if self.filter_sheet(key, "/agent") || self.walk_sheet(key, len) {
            return;
        }
        if key.code != KeyCode::Enter {
            return;
        }
        if self.state.turn_blocks_user_commands() {
            self.close_sheet();
            return;
        }
        let index = self.state.sheet_index.min(len.saturating_sub(1));
        let Some(profile_id) = self
            .state
            .filtered_agents()
            .get(index)
            .map(|profile| profile.id.clone())
        else {
            return;
        };
        if let Some(session_id) = self.state.current_session_id.clone() {
            self.request(RuntimeRequest::SetSessionProfile {
                session_id,
                profile_id,
            });
        } else {
            self.state.selected_profile_id = Some(profile_id);
            self.save_workspace_preferences();
            self.close_sheet();
        }
    }
}
