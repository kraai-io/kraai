use kraai_runtime::Session;

use super::types::{RuntimeRequest, UiMode};
use super::{App, AppState};
use kraai_types::SessionId;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

impl AppState {
    pub(super) fn listed_sessions(&self) -> Vec<&Session> {
        let query = self.sheet.query.as_str().to_lowercase();
        let (here, elsewhere): (Vec<_>, Vec<_>) = self
            .sessions
            .list
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
        !self.sessions.workspace_dir.is_empty()
            && session.workspace_dir == self.sessions.workspace_dir
    }

    pub(super) fn offers_new_chat(&self) -> bool {
        self.sheet.query.is_empty()
    }

    fn highlighted_session(&self) -> Option<SessionId> {
        let sessions = self.listed_sessions();
        let rows = sessions.len() + usize::from(self.offers_new_chat());
        self.sheet
            .index
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

    pub(super) fn replace_sessions(&mut self, sessions: Vec<Session>) {
        let highlighted = (self.state.mode == UiMode::Sessions)
            .then(|| self.state.highlighted_session())
            .flatten();
        self.state.sessions.list = sessions;
        if let Some(index) = highlighted.as_ref().and_then(|id| {
            self.state
                .listed_sessions()
                .iter()
                .position(|session| session.id == *id)
        }) {
            self.state.sheet.index = index + usize::from(self.state.offers_new_chat());
        }
        if self.state.highlighted_session() != highlighted {
            self.state.sheet.armed = false;
        }
    }

    pub(super) fn handle_sessions_key_event(&mut self, key: KeyEvent) {
        let armed = std::mem::take(&mut self.state.sheet.armed);
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
            (KeyCode::Delete, Some(_)) => self.state.sheet.armed = true,
            _ => {}
        }
    }
}
