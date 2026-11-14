use kraai_runtime::{McpAuthState, McpAuthStatus};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, AppState, RuntimeRequest, UiMode, open_external_target};

pub(super) struct BrowserIntent {
    pub(super) request_id: u64,
    sequence: Option<u64>,
}

/// What a server's sheet offers, depending on where its sign-in stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum McpAction {
    SignIn,
    OpenBrowser,
    CopyUrl,
    CancelSignIn,
    LogOut,
}

impl McpAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::SignIn => "Sign in",
            Self::OpenBrowser => "Open the browser again",
            Self::CopyUrl => "Copy the sign-in URL",
            Self::CancelSignIn => "Cancel sign-in",
            Self::LogOut => "Log out",
        }
    }

    fn offered(state: &McpAuthState) -> &'static [Self] {
        match state {
            McpAuthState::Unavailable => &[],
            McpAuthState::SignedOut => &[Self::SignIn],
            McpAuthState::Starting => &[Self::CancelSignIn],
            McpAuthState::Pending { .. } => &[Self::OpenBrowser, Self::CopyUrl, Self::CancelSignIn],
            McpAuthState::Authenticated => &[Self::LogOut],
        }
    }
}

impl AppState {
    /// The servers the server list shows: every server, or the ones the typed text names.
    pub(super) fn listed_mcp_servers(&self) -> Vec<&McpAuthStatus> {
        let query = self.sheet_query.to_lowercase();
        self.mcp_auth
            .values()
            .filter(|status| status.server.to_lowercase().contains(&query))
            .collect()
    }

    /// The actions the open server offers that the typed text names.
    pub(super) fn listed_mcp_actions(&self) -> Vec<McpAction> {
        let query = self.sheet_query.to_lowercase();
        self.mcp_open
            .as_ref()
            .and_then(|server| self.mcp_auth.get(server))
            .map(|status| McpAction::offered(&status.state))
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|action| action.label().to_lowercase().contains(&query))
            .collect()
    }

    fn highlighted_mcp_server(&self) -> Option<String> {
        if self.mode != UiMode::Mcp || self.mcp_open.is_some() {
            return None;
        }
        let servers = self.listed_mcp_servers();
        servers
            .get(self.sheet_index.min(servers.len().checked_sub(1)?))
            .map(|status| status.server.clone())
    }

    fn highlight_mcp_server(&mut self, server: &str) {
        if let Some(index) = self
            .listed_mcp_servers()
            .iter()
            .position(|status| status.server == server)
        {
            self.sheet_index = index;
        }
    }
}

impl App {
    pub(super) fn open_mcp_sheet(&mut self) {
        self.open_sheet(UiMode::Mcp);
        self.state.mcp_open = None;
        self.state.mcp_error = None;
        self.request(RuntimeRequest::GetMcpAuthStatuses);
    }

    pub(super) fn run_mcp_action(&mut self, server: &str, action: McpAction) {
        self.state.mcp_feedback.remove(server);
        let pending_url = self
            .state
            .mcp_auth
            .get(server)
            .and_then(|status| match &status.state {
                McpAuthState::Pending { auth_url } => Some(auth_url.clone()),
                _ => None,
            });
        let request = match action {
            McpAction::SignIn => {
                let request_id = self.state.next_mcp_login_request;
                self.state.next_mcp_login_request = request_id.wrapping_add(1);
                self.state.mcp_browser_intent.insert(
                    server.to_owned(),
                    BrowserIntent {
                        request_id,
                        sequence: None,
                    },
                );
                RuntimeRequest::StartMcpLogin {
                    server: server.to_owned(),
                    request_id,
                }
            }
            McpAction::CancelSignIn => {
                self.state.mcp_browser_intent.remove(server);
                RuntimeRequest::CancelMcpLogin {
                    server: server.to_owned(),
                }
            }
            McpAction::LogOut => {
                self.state.mcp_browser_intent.remove(server);
                RuntimeRequest::LogoutMcp {
                    server: server.to_owned(),
                }
            }
            McpAction::CopyUrl | McpAction::OpenBrowser => {
                let message = match (action, pending_url) {
                    (_, None) => format!("No pending sign-in URL for {server}"),
                    (McpAction::CopyUrl, Some(url)) => {
                        match self.copy_text_to_clipboard(&url, super::feedback::CopyTarget::Auth) {
                            Ok(()) => String::from("MCP sign-in URL copied"),
                            Err(error) => format!("Copy failed: {error}. Sign in at {url}"),
                        }
                    }
                    (_, Some(url)) => browser_feedback(open_external_target(&url)),
                };
                self.state.mcp_feedback.insert(server.to_owned(), message);
                return;
            }
        };
        self.request(request);
    }

    pub(super) fn apply_mcp_auth_status(&mut self, status: McpAuthStatus) {
        self.apply_mcp_auth_status_with(status, open_external_target);
    }

    pub(super) fn acknowledge_mcp_login(
        &mut self,
        request_id: u64,
        mut status: McpAuthStatus,
    ) -> McpAuthStatus {
        if let Some(intent) = self.state.mcp_browser_intent.get_mut(&status.server)
            && intent.request_id == request_id
        {
            intent.sequence = Some(status.sequence);
            if let Some(current) = self.state.mcp_auth.get(&status.server)
                && current.sequence > status.sequence
            {
                status = current.clone();
            }
        }
        status
    }

    pub(super) fn apply_mcp_auth_status_with(
        &mut self,
        status: McpAuthStatus,
        open: impl FnOnce(&str) -> Result<(), String>,
    ) {
        if self
            .state
            .mcp_auth
            .get(&status.server)
            .is_some_and(|current| current.sequence > status.sequence)
        {
            return;
        }
        let browser_intent = self
            .state
            .mcp_browser_intent
            .get(&status.server)
            .and_then(|intent| intent.sequence)
            .is_some_and(|sequence| status.sequence >= sequence);
        let changed = self
            .state
            .mcp_auth
            .get(&status.server)
            .is_none_or(|current| current.sequence < status.sequence);
        if changed {
            self.state.mcp_feedback.remove(&status.server);
        }
        if status.error.is_some() {
            if browser_intent {
                self.state.mcp_browser_intent.remove(&status.server);
            }
        } else if let McpAuthState::Pending { auth_url } = &status.state {
            if browser_intent {
                self.state.mcp_browser_intent.remove(&status.server);
                self.state
                    .mcp_feedback
                    .insert(status.server.clone(), browser_feedback(open(auth_url)));
            }
        } else if browser_intent && !matches!(status.state, McpAuthState::Starting) {
            self.state.mcp_browser_intent.remove(&status.server);
        }
        let highlighted = self.state.highlighted_mcp_server();
        self.state.mcp_auth.insert(status.server.clone(), status);
        if let Some(server) = highlighted {
            self.state.highlight_mcp_server(&server);
        }
    }

    /// Steps out of a server's sheet to the server list, or out of the list to the chat.
    pub(super) fn close_mcp_level(&mut self) {
        match self.state.mcp_open.take() {
            Some(server) => {
                self.open_sheet(UiMode::Mcp);
                self.state.highlight_mcp_server(&server);
            }
            None => self.close_sheet(),
        }
    }

    pub(super) fn handle_mcp_key_event(&mut self, key: KeyEvent) {
        self.state.sheet_notice = None;
        if key.code == KeyCode::Char('r') && key.modifiers == KeyModifiers::CONTROL {
            self.state.mcp_error = None;
            self.request(RuntimeRequest::GetMcpAuthStatuses);
            return;
        }
        let Some(server) = self.state.mcp_open.clone() else {
            let len = self.state.listed_mcp_servers().len();
            if self.filter_sheet(key, "/mcp") || self.walk_sheet(key, len) {
                return;
            }
            if key.code == KeyCode::Enter
                && let Some(server) = self.state.highlighted_mcp_server()
            {
                self.open_sheet(UiMode::Mcp);
                self.state.mcp_feedback.remove(&server);
                self.state.mcp_open = Some(server);
            }
            return;
        };
        if self.state.wants_back(key) {
            self.close_mcp_level();
            return;
        }
        let actions = self.state.listed_mcp_actions();
        if self.edit_sheet_filter(key) || self.walk_sheet(key, actions.len()) {
            return;
        }
        let index = self.state.sheet_index.min(actions.len().saturating_sub(1));
        if key.code != KeyCode::Enter {
            return;
        }
        let Some(action) = actions.get(index).copied() else {
            return;
        };
        if action == McpAction::SignIn && self.state.mcp_browser_intent.contains_key(&server) {
            return;
        }
        self.run_mcp_action(&server, action);
    }
}

fn browser_feedback(opened: Result<(), String>) -> String {
    match opened {
        Ok(()) => String::from("Browser opened. Complete sign-in to continue."),
        Err(error) => format!("Cannot open the browser: {error}. Copy the sign-in URL instead."),
    }
}

pub(super) fn status_label(status: &McpAuthStatus) -> &str {
    match status.state {
        McpAuthState::Unavailable => "no browser sign-in",
        McpAuthState::SignedOut => "signed out",
        McpAuthState::Starting => "preparing sign-in",
        McpAuthState::Pending { .. } => "waiting for sign-in",
        McpAuthState::Authenticated => "signed in",
    }
}
