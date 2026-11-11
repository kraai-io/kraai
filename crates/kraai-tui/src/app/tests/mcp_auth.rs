use super::*;
use crate::app::UiMode;
use crate::app::mcp_auth::McpAction;
use kraai_runtime::{McpAuthState, McpAuthStatus};
use ratatui::crossterm::event::KeyCode;
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

fn status(sequence: u64, state: McpAuthState) -> McpAuthStatus {
    McpAuthStatus {
        server: "example".into(),
        sequence,
        state,
        error: None,
    }
}

fn sign_in(harness: &mut TestHarness) {
    harness.app.run_mcp_action("example", McpAction::SignIn);
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
    sign_in(&mut harness);
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
    sign_in(&mut harness);
    harness
        .app
        .run_mcp_action("example", McpAction::CancelSignIn);
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
    sign_in(&mut harness);
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
    sign_in(&mut harness);
    harness
        .app
        .run_mcp_action("example", McpAction::CancelSignIn);
    sign_in(&mut harness);
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
fn server_sheet_shows_the_sign_in_url_and_why_the_browser_did_not_open() {
    let mut harness = test_harness();
    harness
        .app
        .apply_mcp_auth_status(status(0, McpAuthState::SignedOut));
    harness.app.handle_command("mcp");
    key(&mut harness, KeyCode::Enter);
    key(&mut harness, KeyCode::Enter);
    let starting = harness
        .app
        .acknowledge_mcp_login(0, status(1, McpAuthState::Starting));
    harness.app.apply_mcp_auth_status(starting);
    harness
        .app
        .apply_mcp_auth_status_with(pending(2), |_| Err("missing browser".into()));
    let rows = screen_rows(&harness.app.state, 100, 30);
    let start = rows
        .iter()
        .position(|row| row.starts_with("─ example "))
        .unwrap_or(rows.len());
    assert_eq!(
        rows.get(start..start + 7).unwrap_or_default(),
        [
            format!("─ example {} waiting for sign-in ─", "─".repeat(68)),
            String::from("▌ Open the browser again"),
            String::from("  Copy the sign-in URL"),
            String::from("  Cancel sign-in"),
            String::from("  Complete sign-in in your browser."),
            String::from("  url  https://example.com/oauth"),
            String::from(
                "  Cannot open the browser: missing browser. Copy the sign-in URL instead."
            ),
        ]
    );
    assert!(
        rows.iter()
            .any(|row| row.ends_with("tab or ↑↓ select · enter run · ctrl+r refresh · esc back"))
    );
    assert!(rows.iter().any(|row| row.trim() == "/mcp › example ›"));
}

#[test]
fn background_auth_updates_do_not_change_chat_feedback() {
    let mut harness = test_harness();
    harness.app.state.status = "Unrelated chat status".into();
    let mut failed = status(1, McpAuthState::SignedOut);
    failed.error = Some("Login failed".into());
    harness.app.apply_mcp_auth_status(failed);
    assert_eq!(harness.app.state.status, "Unrelated chat status");
    assert!(harness.app.state.last_error.is_none());
    assert!(!harness.app.state.error_open);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
}

#[test]
fn server_list_opens_a_server_whose_rows_follow_its_sign_in_state() {
    let mut harness = test_harness();
    harness
        .app
        .apply_mcp_auth_status(status(1, McpAuthState::SignedOut));
    let mut other = status(1, McpAuthState::Unavailable);
    other.server = "other".into();
    harness.app.apply_mcp_auth_status(other);
    harness.app.handle_command("mcp");
    harness
        .app
        .handle_runtime_response(RuntimeResponse::McpAuthStatuses(Ok(Vec::new())));
    harness.drain_requests();
    let rows = screen_rows(&harness.app.state, 60, 30);
    let start = rows
        .iter()
        .position(|row| row.starts_with("─ mcp "))
        .unwrap_or(rows.len());
    assert_eq!(
        rows.get(start..start + 3).unwrap_or_default(),
        [
            format!("─ mcp {} 2 servers ─", "─".repeat(42)),
            String::from("▌ example  signed out"),
            String::from("  other    no browser sign-in"),
        ]
    );

    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mcp_open.as_deref(), Some("other"));
    assert!(
        screen_rows(&harness.app.state, 60, 30).contains(&String::from(
            "  This server does not sign in through a browser"
        ))
    );
    key(&mut harness, KeyCode::Enter);
    assert!(harness.drain_requests().is_empty());
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Mcp);
    assert_eq!(harness.app.state.mcp_open, None);
    assert_eq!(harness.app.state.sheet_index, 1);

    type_text(&mut harness, "exam");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mcp_open.as_deref(), Some("example"));
    assert!(harness.app.state.sheet_query.is_empty());
    key(&mut harness, KeyCode::Enter);
    key(&mut harness, KeyCode::Enter);
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::StartMcpLogin { server, .. }] if server == "example")
    );
    harness
        .app
        .apply_mcp_auth_status(status(2, McpAuthState::Starting));
    key(&mut harness, KeyCode::Enter);
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::CancelMcpLogin { server }] if server == "example")
    );
    harness.app.apply_mcp_auth_status(pending(3));
    type_text(&mut harness, "cancel");
    assert_eq!(
        harness.app.state.listed_mcp_actions(),
        [McpAction::CancelSignIn]
    );
    key(&mut harness, KeyCode::Enter);
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::CancelMcpLogin { server }] if server == "example")
    );
    harness
        .app
        .apply_mcp_auth_status(status(4, McpAuthState::Authenticated));
    ctrl(&mut harness, 'u');
    key(&mut harness, KeyCode::Enter);
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::LogoutMcp { server }] if server == "example")
    );
    ctrl(&mut harness, 'r');
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::GetMcpAuthStatuses]
    ));
    key(&mut harness, KeyCode::Backspace);
    assert_eq!(harness.app.state.mcp_open, None);
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
}

#[test]
fn duplicate_pending_snapshot_preserves_browser_failure_feedback() {
    let mut harness = test_harness();
    sign_in(&mut harness);
    let starting = harness
        .app
        .acknowledge_mcp_login(0, status(1, McpAuthState::Starting));
    harness.app.apply_mcp_auth_status(starting);
    harness
        .app
        .apply_mcp_auth_status_with(pending(2), |_| Err("no browser".into()));
    harness.app.apply_mcp_auth_status(pending(2));
    assert!(
        harness
            .app
            .state
            .mcp_feedback
            .get("example")
            .is_some_and(|message| message.contains("no browser"))
    );
    harness
        .app
        .apply_mcp_auth_status(status(3, McpAuthState::Authenticated));
    assert!(!harness.app.state.mcp_feedback.contains_key("example"));
}

#[test]
fn mcp_view_handles_empty_and_small_terminals() {
    let mut harness = test_harness();
    harness.app.handle_command("mcp");
    harness
        .app
        .handle_runtime_response(RuntimeResponse::McpAuthStatuses(Ok(Vec::new())));
    for (width, height) in [(100, 30), (20, 8), (1, 1), (0, 0)] {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        (&harness.app.state).render(area, &mut buffer);
        if width == 100 {
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("No MCP servers configured"));
        }
    }
}
