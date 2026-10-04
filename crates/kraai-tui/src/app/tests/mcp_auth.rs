use super::*;
use kraai_runtime::{McpAuthState, McpAuthStatus};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

fn status(sequence: u64, state: McpAuthState) -> McpAuthStatus {
    McpAuthStatus {
        server: "example".into(),
        sequence,
        state,
        error: None,
    }
}

fn pending(sequence: u64) -> McpAuthStatus {
    status(
        sequence,
        McpAuthState::Pending {
            auth_url: "https://example.com/oauth".into(),
        },
    )
}

#[test]
fn pending_status_only_opens_browser_once_after_explicit_login() {
    let mut harness = test_harness();
    let opened = std::cell::Cell::new(0);
    let open = |_: &str| {
        opened.set(opened.get() + 1);
        Ok(())
    };
    harness.app.apply_mcp_auth_status_with(pending(1), open);
    assert_eq!(opened.get(), 0);
    harness.app.handle_command("mcp login example");
    harness.app.apply_mcp_auth_status_with(pending(1), open);
    assert_eq!(opened.get(), 0);
    let starting = harness
        .app
        .acknowledge_mcp_login(0, status(2, McpAuthState::Starting));
    harness.app.apply_mcp_auth_status_with(starting, open);
    harness.app.apply_mcp_auth_status_with(pending(3), open);
    harness.app.apply_mcp_auth_status_with(pending(3), open);
    assert_eq!(opened.get(), 1);
}

#[test]
fn cancel_clears_browser_intent_and_late_pending_cannot_replace_completion() {
    let mut harness = test_harness();
    let opened = std::cell::Cell::new(false);
    let open = |_: &str| {
        opened.set(true);
        Ok(())
    };
    harness.app.handle_command("mcp login example");
    harness.app.handle_command("mcp cancel example");
    harness.app.apply_mcp_auth_status_with(pending(2), open);
    assert!(!opened.get());
    harness
        .app
        .apply_mcp_auth_status(status(3, McpAuthState::Authenticated));
    harness.app.apply_mcp_auth_status_with(pending(2), open);
    assert!(matches!(
        harness
            .app
            .state
            .mcp_auth
            .get("example")
            .map(|status| &status.state),
        Some(McpAuthState::Authenticated)
    ));
}

#[test]
fn delayed_start_response_opens_cached_pending_after_initial_snapshot() {
    let mut harness = test_harness();
    let opened = std::cell::Cell::new(0);
    let open = |_: &str| {
        opened.set(opened.get() + 1);
        Ok(())
    };
    harness.app.handle_command("mcp login example");
    harness
        .app
        .apply_mcp_auth_status(status(0, McpAuthState::SignedOut));
    harness.app.apply_mcp_auth_status_with(pending(2), open);
    assert_eq!(opened.get(), 0);
    let current = harness
        .app
        .acknowledge_mcp_login(0, status(1, McpAuthState::Starting));
    harness.app.apply_mcp_auth_status_with(current, open);
    assert_eq!(opened.get(), 1);
}

#[test]
fn rapid_login_cancel_login_only_opens_latest_attempt() {
    let mut harness = test_harness();
    let opened = std::cell::Cell::new(0);
    let open = |_: &str| {
        opened.set(opened.get() + 1);
        Ok(())
    };
    harness.app.handle_command("mcp login example");
    harness.app.handle_command("mcp cancel example");
    harness.app.handle_command("mcp login example");
    harness
        .app
        .handle_runtime_response(RuntimeResponse::StartMcpLogin {
            server: "example".into(),
            request_id: 0,
            result: Err(kraai_runtime::RuntimeError::new(
                kraai_runtime::RuntimeErrorKind::Internal,
                "old failure",
            )),
        });
    assert!(harness.app.state.last_error.is_none());
    let old = harness
        .app
        .acknowledge_mcp_login(0, status(1, McpAuthState::Starting));
    harness.app.apply_mcp_auth_status_with(old, open);
    harness.app.apply_mcp_auth_status_with(pending(2), open);
    harness
        .app
        .apply_mcp_auth_status(status(3, McpAuthState::SignedOut));
    let current = harness
        .app
        .acknowledge_mcp_login(1, status(4, McpAuthState::Starting));
    harness.app.apply_mcp_auth_status_with(current, open);
    harness.app.apply_mcp_auth_status_with(pending(5), open);
    assert_eq!(opened.get(), 1);
}

#[test]
fn mcp_view_renders_server_status_and_browser_failure_url() {
    let mut harness = test_harness();
    harness.app.handle_command("mcp login example");
    let starting = harness
        .app
        .acknowledge_mcp_login(0, status(1, McpAuthState::Starting));
    harness.app.apply_mcp_auth_status(starting);
    harness
        .app
        .apply_mcp_auth_status_with(pending(2), |_| Err("missing browser".into()));
    let area = Rect::new(0, 0, 100, 30);
    let mut buffer = Buffer::empty(area);
    (&harness.app.state).render(area, &mut buffer);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("example: waiting for sign-in"));
    assert!(text.contains("https://example.com/oauth"));
    assert!(text.contains("missing browser"));
    assert!(text.contains("/mcp copy <server>"));
}

#[test]
fn mcp_commands_validate_arguments_before_dispatch() {
    let mut harness = test_harness();
    harness.app.handle_command("mcp login example extra");
    assert!(harness.drain_requests().is_empty());
    harness.app.handle_command("mcp");
    harness.app.handle_command("mcp login example");
    harness.app.handle_command("mcp cancel example");
    harness.app.handle_command("mcp logout example");
    assert!(matches!(harness.drain_requests().as_slice(), [
        RuntimeRequest::GetMcpAuthStatuses,
        RuntimeRequest::StartMcpLogin { server: login, .. },
        RuntimeRequest::CancelMcpLogin { server: cancel },
        RuntimeRequest::LogoutMcp { server: logout },
    ] if login == "example" && cancel == login && logout == login));
}
