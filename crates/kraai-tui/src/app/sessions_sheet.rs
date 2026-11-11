use kraai_runtime::Session;
use kraai_types::SessionId;

use super::*;

impl AppState {
    /// The sessions the session list shows, with the ones from this workspace first.
    pub(super) fn listed_sessions(&self) -> Vec<&Session> {
        let query = self.sheet_query.to_lowercase();
        let (here, elsewhere): (Vec<_>, Vec<_>) = self
            .sessions
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
            .partition(|session| self.in_this_workspace(session));
        here.into_iter().chain(elsewhere).collect()
    }

    pub(super) fn in_this_workspace(&self, session: &Session) -> bool {
        !self.workspace_dir.is_empty() && session.workspace_dir == self.workspace_dir
    }

    /// The session list starts with a row that opens a new chat until something is typed.
    pub(super) fn offers_new_chat(&self) -> bool {
        self.sheet_query.is_empty()
    }

    fn highlighted_session(&self) -> Option<SessionId> {
        let sessions = self.listed_sessions();
        let rows = sessions.len() + usize::from(self.offers_new_chat());
        self.sheet_index
            .min(rows.checked_sub(1)?)
            .checked_sub(usize::from(self.offers_new_chat()))
            .and_then(|index| sessions.get(index))
            .map(|session| session.id.clone())
    }
}

impl App {
    pub(super) fn open_sessions_sheet(&mut self) {
        self.open_sheet(UiMode::Sessions);
        self.request(RuntimeRequest::ListSessions);
    }

    /// Replaces the known sessions and keeps the session list on the session it highlighted.
    pub(super) fn replace_sessions(&mut self, sessions: Vec<Session>) {
        let highlighted = (self.state.mode == UiMode::Sessions)
            .then(|| self.state.highlighted_session())
            .flatten();
        self.state.sessions = sessions;
        if let Some(index) = highlighted.and_then(|id| {
            self.state
                .listed_sessions()
                .iter()
                .position(|session| session.id == id)
        }) {
            self.state.sheet_index = index + usize::from(self.state.offers_new_chat());
        }
    }

    pub(super) fn handle_sessions_key_event(&mut self, key: KeyEvent) {
        let armed = std::mem::take(&mut self.state.sheet_armed);
        let rows = self.state.listed_sessions().len() + usize::from(self.state.offers_new_chat());
        if self.filter_sheet(key, "/sessions") || self.walk_sheet(key, rows) {
            return;
        }
        let session = self.state.highlighted_session();
        match (key.code, session) {
            (KeyCode::Enter, Some(session_id)) => self.load_session(session_id),
            (KeyCode::Enter, None) if rows > 0 => self.start_new_chat(),
            (KeyCode::Delete, Some(session_id)) if armed => {
                self.request(RuntimeRequest::DeleteSession { session_id });
            }
            (KeyCode::Delete, Some(_)) => self.state.sheet_armed = true,
            _ => {}
        }
    }
}
