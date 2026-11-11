use kraai_execution_types::{
    EnvironmentPolicy, EscalationPolicy, NushellStartup, PathPolicy, SandboxCapabilities,
    SandboxCapability,
};
use kraai_runtime::{AgentProfileSource, AgentProfileSummary};
use kraai_types::SessionId;

use super::*;
use crate::app::{KeyCode, UiMode};

fn agent(id: &str, name: &str, description: &str) -> AgentProfileSummary {
    AgentProfileSummary {
        id: id.into(),
        display_name: name.into(),
        description: description.into(),
        commands: Vec::new(),
        capabilities: SandboxCapabilities::new([SandboxCapability::WorkspaceWrite]),
        escalation_policy: EscalationPolicy::Prompt,
        environment: EnvironmentPolicy::Minimal,
        nushell_startup: NushellStartup::Clean,
        path: PathPolicy::Inherit,
        source: AgentProfileSource::BuiltIn,
    }
}

fn with_agents(harness: &mut TestHarness) {
    harness.app.state.agent_profiles = vec![
        agent("coding", "Coding", "Writes code"),
        agent("review", "Review", "Reads code"),
    ];
    harness.app.state.selected_profile_id = Some("review".into());
}

fn session(id: &str, title: &str, workspace: &str) -> Session {
    let mut session = session_snapshot(None).session;
    session.id = SessionId::new(id);
    session.title = Some(title.into());
    session.workspace_dir = workspace.into();
    session.updated_at = u64::MAX;
    session
}

fn with_sessions(harness: &mut TestHarness) {
    let state = &mut harness.app.state;
    state.workspace_dir = String::from("/home/me/code/kraai");
    state.home_dir = String::from("/home/me");
    let mut other = session("other", "Other work", "/home/me/elsewhere");
    other.is_running = true;
    let mut waiting = session("waiting", "Add sign-in", "/home/me/code/kraai");
    waiting.waiting_for_approval = true;
    state.sessions = vec![
        other,
        session("fetch", "Fix fetch", "/home/me/code/kraai"),
        waiting,
    ];
    state.current_session_id = Some(SessionId::new("fetch"));
}

/// The rows of the open sheet, from its rule to the line above the status line.
fn sheet(harness: &TestHarness, width: u16) -> Vec<String> {
    let rows = screen_rows(&harness.app.state, width, 30);
    let start = rows
        .iter()
        .position(|row| row.starts_with("─ "))
        .unwrap_or(rows.len());
    let end = rows
        .iter()
        .rposition(|row| row.contains(" · esc ") || row.contains("any other key"))
        .unwrap_or(rows.len());
    rows.get(start..end.saturating_sub(1))
        .unwrap_or_default()
        .to_vec()
}

fn spaced(left: &str, right: &str, width: usize) -> String {
    let used = left.chars().count() + right.chars().count() + 1;
    format!("{left}{}{right}", " ".repeat(width.saturating_sub(used)))
}

#[test]
fn agent_sheet_opens_on_the_agent_in_use_and_describes_the_highlighted_one() {
    let mut harness = test_harness();
    with_agents(&mut harness);
    harness.app.handle_command("agent");
    assert_eq!(harness.app.state.mode, UiMode::Agent);
    assert_eq!(harness.app.state.sheet_index, 1);
    assert_eq!(
        sheet(&harness, 50),
        [
            format!("─ agent {} 2 agents ─", "─".repeat(31)),
            String::from("  Coding  Writes code"),
            spaced("▌ Review  Reads code", "current", 50),
            String::new(),
            String::from("  id          review"),
            String::from("  sandbox     workspace-read  workspace-write"),
            String::from("  escalation  prompt"),
            String::from("  source      built-in"),
        ]
    );
    let rows = screen_rows(&harness.app.state, 50, 30);
    assert!(
        rows.iter()
            .any(|row| row.ends_with("tab or ↑↓ select · enter use · esc close"))
    );
    assert!(rows.iter().any(|row| row.trim() == "/agent"));

    key(&mut harness, KeyCode::Tab);
    assert_eq!(harness.app.state.sheet_index, 0);
    assert!(sheet(&harness, 50).contains(&String::from("  id          coding")));
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.selected_profile_id.as_deref(),
        Some("review")
    );
}

#[test]
fn typing_filters_agents_and_enter_uses_the_highlighted_one() {
    let mut harness = test_harness();
    with_agents(&mut harness);
    harness.app.handle_command("agent");
    type_text(&mut harness, "writes");
    assert_eq!(
        sheet(&harness, 50).get(..2).unwrap_or_default(),
        [
            format!("─ agent {} 1 agent ─", "─".repeat(32)),
            String::from("▌ Coding  Writes code"),
        ]
    );
    type_text(&mut harness, "zz");
    assert_eq!(
        sheet(&harness, 50).get(1).map(String::as_str),
        Some("  No agents match")
    );
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Agent);
    key(&mut harness, KeyCode::Backspace);
    key(&mut harness, KeyCode::Backspace);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.selected_profile_id.as_deref(),
        Some("coding")
    );
    assert!(harness.app.state.sheet_query.is_empty());
    assert!(harness.app.state.input.is_empty());
}

#[test]
fn choosing_an_agent_in_a_session_waits_for_the_runtime_to_accept_it() {
    let mut harness = test_harness();
    with_agents(&mut harness);
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness.app.handle_command("agent");
    harness.drain_requests();
    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Enter);
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::SetSessionProfile { profile_id, .. }] if profile_id == "coding"
    ));
    assert_eq!(harness.app.state.mode, UiMode::Agent);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionProfile {
            session_id: SessionId::new("session"),
            profile_id: String::from("coding"),
            result: Ok(()),
        });
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.selected_profile_id.as_deref(),
        Some("coding")
    );
}

#[test]
fn backspace_on_an_empty_filter_hands_the_command_back_to_the_composer() {
    for command in ["agent", "sessions"] {
        let mut harness = test_harness();
        harness.app.handle_command(command);
        assert_ne!(harness.app.state.mode, UiMode::Chat);
        key(&mut harness, KeyCode::Backspace);
        assert_eq!(harness.app.state.mode, UiMode::Chat);
        assert_eq!(harness.app.state.input, format!("/{command}"));
    }
}

#[test]
fn sessions_from_this_workspace_come_first_and_others_show_where_they_ran() {
    let mut harness = test_harness();
    with_sessions(&mut harness);
    harness.app.handle_command("sessions");
    assert_eq!(harness.app.state.mode, UiMode::Sessions);
    assert_eq!(
        sheet(&harness, 60),
        [
            format!("─ sessions {} 3 sessions ─", "─".repeat(36)),
            String::from("▌ New chat"),
            String::from("  ~/code/kraai"),
            spaced("  Fix fetch", "current   0s", 60),
            spaced("  Add sign-in", "needs approval   0s", 60),
            String::from("  elsewhere"),
            spaced("  Other work   ~/elsewhere", "running   0s", 60),
        ]
    );
    let rows = screen_rows(&harness.app.state, 80, 30);
    assert!(
        rows.iter()
            .any(|row| row.ends_with("tab or ↑↓ select · enter open · del delete · esc close"))
    );

    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.current_session_id.is_none());
}

#[test]
fn typing_filters_sessions_and_enter_opens_the_first_match() {
    let mut harness = test_harness();
    with_sessions(&mut harness);
    harness.app.handle_command("sessions");
    harness.drain_requests();
    type_text(&mut harness, "sign");
    assert_eq!(
        sheet(&harness, 60),
        [
            format!("─ sessions {} 1 session ─", "─".repeat(37)),
            String::from("  ~/code/kraai"),
            spaced("▌ Add sign-in", "needs approval   0s", 60),
        ]
    );
    key(&mut harness, KeyCode::Enter);
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::LoadSession { session_id, .. }] if session_id.as_str() == "waiting"
    ));
    type_text(&mut harness, "zz");
    assert_eq!(
        sheet(&harness, 60).get(1).map(String::as_str),
        Some("  No sessions match")
    );
    key(&mut harness, KeyCode::Enter);
    key(&mut harness, KeyCode::Delete);
    assert!(harness.drain_requests().is_empty());
    assert!(!harness.app.state.sheet_armed);
}

#[test]
fn deleting_a_session_takes_the_key_twice_and_any_other_key_keeps_it() {
    let mut harness = test_harness();
    with_sessions(&mut harness);
    harness.app.handle_command("sessions");
    harness.drain_requests();
    key(&mut harness, KeyCode::Delete);
    assert!(!harness.app.state.sheet_armed);

    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Delete);
    assert!(harness.drain_requests().is_empty());
    assert_eq!(
        sheet(&harness, 60).get(3),
        Some(&spaced("▌ Fix fetch", "del again to delete   0s", 60))
    );
    assert!(
        screen_rows(&harness.app.state, 60, 30)
            .iter()
            .any(|row| row.ends_with("del delete this session · any other key keeps it"))
    );
    key(&mut harness, KeyCode::Down);
    assert!(!harness.app.state.sheet_armed);
    assert_eq!(harness.app.state.sheet_index, 2);
    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Delete);
    key(&mut harness, KeyCode::Char('f'));
    assert!(!harness.app.state.sheet_armed);
    assert!(harness.drain_requests().is_empty());
    key(&mut harness, KeyCode::Backspace);

    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Delete);
    key(&mut harness, KeyCode::Delete);
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::DeleteSession { session_id }] if session_id.as_str() == "fetch"
    ));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::DeleteSession {
            session_id: SessionId::new("fetch"),
            result: Ok(()),
        });
    assert_eq!(harness.app.state.sessions.len(), 2);
}

#[test]
fn a_refreshed_session_list_keeps_the_highlight_on_the_same_session() {
    let mut harness = test_harness();
    with_sessions(&mut harness);
    harness.app.handle_command("sessions");
    key(&mut harness, KeyCode::Up);
    assert_eq!(harness.app.state.sheet_index, 3);
    let mut sessions = harness.app.state.sessions.clone();
    sessions.insert(0, session("new", "Newer", "/home/me/code/kraai"));
    sessions.push(session("far", "Far away", "/srv"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Sessions(Ok(sessions)));
    assert_eq!(harness.app.state.sheet_index, 4);
    assert_eq!(
        sheet(&harness, 60).get(7),
        Some(&spaced("▌ Other work   ~/elsewhere", "running   0s", 60))
    );
}
