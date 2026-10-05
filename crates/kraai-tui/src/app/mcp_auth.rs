use kraai_runtime::{McpAuthState, McpAuthStatus};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::{App, RuntimeRequest, open_external_target};

pub(super) struct BrowserIntent {
    pub(super) request_id: u64,
    sequence: Option<u64>,
}

impl App {
    pub(super) fn handle_mcp_command(&mut self, args: Vec<&str>) {
        self.state.mode = super::UiMode::Mcp;
        self.state.mcp_scroll.set(0);
        self.state.mcp_error = None;
        if let Some(server) = args.get(1) {
            self.state.mcp_selected = Some((*server).into());
            self.state.mcp_feedback.remove(*server);
        }
        let request = match args.as_slice() {
            [] | ["status"] => RuntimeRequest::GetMcpAuthStatuses,
            ["login", server] => {
                let request_id = self.state.next_mcp_login_request;
                self.state.next_mcp_login_request = request_id.wrapping_add(1);
                self.state.mcp_browser_intent.insert(
                    (*server).into(),
                    BrowserIntent {
                        request_id,
                        sequence: None,
                    },
                );
                RuntimeRequest::StartMcpLogin {
                    server: (*server).into(),
                    request_id,
                }
            }
            ["cancel", server] => {
                self.state.mcp_browser_intent.remove(*server);
                RuntimeRequest::CancelMcpLogin {
                    server: (*server).into(),
                }
            }
            ["logout", server] => {
                self.state.mcp_browser_intent.remove(*server);
                RuntimeRequest::LogoutMcp {
                    server: (*server).into(),
                }
            }
            ["copy", server] => {
                let url = self
                    .state
                    .mcp_auth
                    .get(*server)
                    .and_then(|status| match &status.state {
                        McpAuthState::Pending { auth_url } => Some(auth_url.clone()),
                        _ => None,
                    });
                let message = match url {
                    Some(url) => {
                        match self.copy_text_to_clipboard(&url, super::feedback::CopyTarget::Auth) {
                            Ok(()) => String::from("MCP sign-in URL copied"),
                            Err(error) => format!("Copy failed: {error}. Sign in at {url}"),
                        }
                    }
                    None => format!("No pending sign-in URL for {server}"),
                };
                self.state.mcp_feedback.insert((*server).into(), message);
                return;
            }
            _ => {
                self.state.mcp_error = Some(String::from(
                    "Use /mcp status, /mcp login <server>, /mcp cancel <server>, /mcp logout <server>, or /mcp copy <server>",
                ));
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
                let message = match open(auth_url) {
                    Ok(()) => String::from("Browser opened. Complete sign-in to continue."),
                    Err(error) => {
                        format!("Cannot open browser: {error}. Press y to copy the sign-in URL.")
                    }
                };
                self.state
                    .mcp_feedback
                    .insert(status.server.clone(), message);
            }
        } else if browser_intent && !matches!(status.state, McpAuthState::Starting) {
            self.state.mcp_browser_intent.remove(&status.server);
        }
        if self.state.mcp_selected.is_none() {
            self.state.mcp_selected = Some(status.server.clone());
        }
        self.state.mcp_auth.insert(status.server.clone(), status);
    }

    pub(super) fn handle_mcp_key_event(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.state.mode = super::UiMode::Chat,
            KeyCode::Char('r') => {
                self.state.mcp_error = None;
                self.request(RuntimeRequest::GetMcpAuthStatuses);
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => {
                let servers: Vec<_> = self.state.mcp_auth.keys().cloned().collect();
                if servers.is_empty() {
                    return;
                }
                let index = servers
                    .iter()
                    .position(|name| Some(name) == self.state.mcp_selected.as_ref())
                    .unwrap_or(0);
                let next = match key.code {
                    KeyCode::Up => index.saturating_sub(1),
                    KeyCode::Down => (index + 1).min(servers.len() - 1),
                    KeyCode::End => servers.len() - 1,
                    _ => 0,
                };
                self.state.mcp_selected = servers.get(next).cloned();
                self.state.mcp_scroll.set(0);
            }
            KeyCode::PageUp => self
                .state
                .mcp_scroll
                .set(self.state.mcp_scroll.get().saturating_sub(10)),
            KeyCode::PageDown => self
                .state
                .mcp_scroll
                .set(self.state.mcp_scroll.get().saturating_add(10)),
            _ => {
                let Some(server) = self.state.mcp_selected.clone() else {
                    return;
                };
                let Some(status) = self.state.mcp_auth.get(&server) else {
                    return;
                };
                let operation = match (&status.state, key.code) {
                    (McpAuthState::SignedOut, KeyCode::Enter | KeyCode::Char('b')) => "login",
                    (McpAuthState::Starting | McpAuthState::Pending { .. }, KeyCode::Char('x')) => {
                        "cancel"
                    }
                    (McpAuthState::Authenticated, KeyCode::Char('l')) => "logout",
                    (McpAuthState::Pending { .. }, KeyCode::Char('y')) => "copy",
                    (McpAuthState::Pending { auth_url }, KeyCode::Char('o') | KeyCode::Enter) => {
                        let message = match open_external_target(auth_url) {
                            Ok(()) => String::from("Browser opened. Complete sign-in to continue."),
                            Err(error) => format!(
                                "Cannot open browser: {error}. Press y to copy the sign-in URL."
                            ),
                        };
                        self.state.mcp_feedback.insert(server, message);
                        return;
                    }
                    _ => return,
                };
                if operation == "login" && self.state.mcp_browser_intent.contains_key(&server) {
                    return;
                }
                self.handle_mcp_command(vec![operation, &server]);
            }
        }
    }
}

pub(super) fn status_label(status: &McpAuthStatus) -> &str {
    match status.state {
        McpAuthState::Unavailable => "no browser login",
        McpAuthState::SignedOut => "signed out",
        McpAuthState::Starting => "preparing sign-in",
        McpAuthState::Pending { .. } => "waiting for sign-in",
        McpAuthState::Authenticated => "signed in",
    }
}
