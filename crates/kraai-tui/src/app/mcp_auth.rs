use kraai_runtime::{McpAuthState, McpAuthStatus};

use super::{App, RuntimeRequest, open_external_target};

pub(super) struct BrowserIntent {
    pub(super) request_id: u64,
    sequence: Option<u64>,
}

impl App {
    pub(super) fn handle_mcp_command(&mut self, args: Vec<&str>) {
        self.state.mode = super::UiMode::Mcp;
        self.state.mcp_scroll.set(0);
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
                self.state.status = match url {
                    Some(url) => {
                        match self.copy_text_to_clipboard(&url, super::feedback::CopyTarget::Auth) {
                            Ok(()) => String::from("MCP sign-in URL copied"),
                            Err(error) => format!("Copy failed: {error}. Sign in at {url}"),
                        }
                    }
                    None => format!("No pending sign-in URL for {server}"),
                };
                return;
            }
            _ => {
                self.state.status = String::from(
                    "Use /mcp status, /mcp login <server>, /mcp cancel <server>, /mcp logout <server>, or /mcp copy <server>",
                );
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
        self.state.status = format!("{}: {}", status.server, status_label(&status));
        if let Some(error) = &status.error {
            if browser_intent {
                self.state.mcp_browser_intent.remove(&status.server);
            }
            self.set_error(format!("MCP {}: {error}", status.server));
        } else if let McpAuthState::Pending { auth_url } = &status.state {
            self.state.status = if browser_intent {
                self.state.mcp_browser_intent.remove(&status.server);
                match open(auth_url) {
                    Ok(()) => format!(
                        "Opened sign-in for {}. /mcp copy {} copies the URL. {auth_url}",
                        status.server, status.server
                    ),
                    Err(error) => format!(
                        "Cannot open browser: {error}. Sign in at {auth_url}. /mcp copy {} copies the URL",
                        status.server
                    ),
                }
            } else {
                format!(
                    "{}: waiting for sign-in. /mcp copy {} copies the URL. {auth_url}",
                    status.server, status.server
                )
            };
        } else if browser_intent && !matches!(status.state, McpAuthState::Starting) {
            self.state.mcp_browser_intent.remove(&status.server);
        }
        self.state.mcp_auth.insert(status.server.clone(), status);
    }

    pub(super) fn mcp_auth_summary(&self) -> String {
        if self.state.mcp_auth.is_empty() {
            return String::from("No MCP servers configured");
        }
        self.state
            .mcp_auth
            .values()
            .map(|status| format!("{}: {}", status.server, status_label(status)))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

pub(super) fn status_label(status: &McpAuthStatus) -> &str {
    if let Some(error) = &status.error {
        return error;
    }
    match status.state {
        McpAuthState::Unavailable => "OAuth unavailable",
        McpAuthState::SignedOut => "signed out",
        McpAuthState::Starting => "preparing sign-in",
        McpAuthState::Pending { .. } => "waiting for sign-in",
        McpAuthState::Authenticated => "signed in",
    }
}
