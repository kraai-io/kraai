use std::collections::HashMap;

#[test]
fn wheel_scrolling_moves_three_lines_and_resumes_following_at_bottom() {
    use super::{KeyModifiers, MouseEvent, MouseEventKind};

    let mut harness = test_harness();
    harness.app.state.chat_render_cache.borrow_mut().total_lines = 100;
    harness.app.state.chat_viewport_height = 20;
    let wheel = |kind| MouseEvent {
        kind,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };

    harness
        .app
        .handle_mouse_event(wheel(MouseEventKind::ScrollUp));
    assert_eq!(harness.app.state.scroll, 77);
    assert!(!harness.app.state.auto_scroll);
    harness
        .app
        .handle_mouse_event(wheel(MouseEventKind::ScrollUp));
    assert_eq!(harness.app.state.scroll, 74);
    harness
        .app
        .handle_mouse_event(wheel(MouseEventKind::ScrollDown));
    assert_eq!(harness.app.state.scroll, 77);
    harness
        .app
        .handle_mouse_event(wheel(MouseEventKind::ScrollDown));
    assert_eq!(harness.app.state.scroll, 80);
    assert!(harness.app.state.auto_scroll);

    harness.app.scroll_chat_to_top();
    harness
        .app
        .handle_mouse_event(wheel(MouseEventKind::ScrollUp));
    assert_eq!(harness.app.state.scroll, 0);
    assert!(!harness.app.state.auto_scroll);

    harness.app.state.mode = super::UiMode::Help;
    harness
        .app
        .handle_mouse_event(wheel(MouseEventKind::ScrollDown));
    assert_eq!(harness.app.state.scroll, 0);
}

use crossbeam_channel::{Receiver, unbounded};
use kraai_runtime::{
    AgentProfilesState, Event, PendingScriptInfo, RuntimeEvent, Session, SessionActivity,
    SessionSnapshot,
};
use kraai_types::{Message, MessageId, MessageStatus, SessionId};

use super::runtime_bridge::RuntimeEventBridgeMessage;
use super::{
    App, AppState, RuntimeRequest, RuntimeResponse, ScriptApprovalAction, ScriptPhase,
    StartupOptions,
};

struct TestHarness {
    app: App,
    requests_rx: Receiver<RuntimeRequest>,
}

#[test]
fn escape_cancels_an_executing_script() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness.app.state.script_phase = ScriptPhase::Executing;

    harness.app.handle_key_event(super::KeyEvent::new(
        super::KeyCode::Esc,
        super::KeyModifiers::NONE,
    ));

    assert!(matches!(
        harness.requests_rx.try_recv(),
        Ok(RuntimeRequest::CancelStream { session_id }) if session_id == SessionId::new("session")
    ));
    assert_eq!(harness.app.state.script_phase, ScriptPhase::Executing);
}

fn test_harness() -> TestHarness {
    let (_event_tx, event_rx) = unbounded();
    let (runtime_tx, requests_rx) = unbounded();
    let (_responses_tx, runtime_rx) = unbounded();
    TestHarness {
        app: App {
            event_rx,
            runtime_tx,
            runtime_rx,
            clipboard: None,
            startup_options: StartupOptions::default(),
            persist_workspace_preferences: false,
            startup_message_sent: false,
            startup_sync: super::StartupSync::Complete,
            state: AppState::default(),
            last_statusline_animation_tick: None,
            last_runtime_event_sequence: 0,
            last_session_event_sequences: HashMap::new(),
            session_snapshot_sequences: HashMap::new(),
            event_lag_session_resync_pending: false,
            event_lag_script_resync_pending: false,
            runtime_bridge_connected: true,
            runtime_bridge_error: None,
        },
        requests_rx,
    }
}

impl TestHarness {
    fn drain_requests(&self) -> Vec<RuntimeRequest> {
        let mut requests = Vec::new();
        while let Ok(request) = self.requests_rx.try_recv() {
            requests.push(request);
        }
        requests
    }
}

fn pending_script(execution_id: &str) -> PendingScriptInfo {
    PendingScriptInfo {
        execution_id: execution_id.to_string(),
        call_id: String::from("call"),
        source: String::from("^cargo test"),
        requested_capabilities: vec![String::from("workspace-write")],
        capability_additions: vec![String::from("workspace-write")],
        timeout_millis: 30_000,
    }
}

fn session_snapshot(pending_script: Option<PendingScriptInfo>) -> SessionSnapshot {
    session_snapshot_at(0, pending_script)
}

fn session_snapshot_at(
    event_sequence: u64,
    pending_script: Option<PendingScriptInfo>,
) -> SessionSnapshot {
    let activity = if pending_script.is_some() {
        SessionActivity::AwaitingApproval
    } else {
        SessionActivity::Idle
    };
    SessionSnapshot {
        requests: std::collections::BTreeMap::new(),
        turn_timer: Default::default(),
        event_sequence,
        session: Session {
            id: SessionId::new("session"),
            tip_id: None,
            workspace_dir: String::from("/workspace"),
            created_at: 0,
            updated_at: 0,
            title: None,
            selected_profile_id: Some(String::from("coding")),
            selected_model: None,
            profile_locked: pending_script.is_some(),
            waiting_for_approval: pending_script.is_some(),
            is_streaming: false,
            is_running: pending_script.is_some(),
        },
        history: std::collections::BTreeMap::new(),
        older_history: None,
        context_usage: None,
        pending_script,
        profiles: AgentProfilesState {
            profiles: Vec::new(),
            warnings: Vec::new(),
            selected_profile_id: Some(String::from("coding")),
            profile_locked: false,
        },
        activity,
        queued_messages: 0,
    }
}

#[test]
fn snapshot_watermarks_only_suppress_covered_events_for_the_same_session() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("other-session"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: SessionId::new("session"),
            result: Box::new(Ok(session_snapshot_at(10, None))),
        });

    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Event(RuntimeEvent {
            sequence: 9,
            event: Event::ContextStateChanged {
                session_id: SessionId::new("other-session"),
                notifications: vec![String::from("other session event was delivered")],
            },
        }));
    assert_eq!(
        harness.app.state.status,
        "other session event was delivered"
    );

    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Event(RuntimeEvent {
            sequence: 10,
            event: Event::ContextStateChanged {
                session_id: SessionId::new("session"),
                notifications: vec![String::from("covered event should be ignored")],
            },
        }));
    assert_ne!(harness.app.state.status, "covered event should be ignored");
}

#[test]
fn snapshot_is_rejected_only_when_a_newer_event_for_its_session_was_applied() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Event(RuntimeEvent {
            sequence: 11,
            event: Event::ContextStateChanged {
                session_id: SessionId::new("session"),
                notifications: vec![String::from("newer session state")],
            },
        }));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: SessionId::new("session"),
            result: Box::new(Ok(session_snapshot_at(10, None))),
        });

    assert_eq!(harness.app.state.status, "newer session state");
    assert!(
        !harness
            .app
            .session_snapshot_sequences
            .contains_key(&SessionId::new("session"))
    );
}

#[test]
fn script_approval_event_enters_single_script_decision_state() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness
        .app
        .handle_runtime_event(Event::ScriptApprovalRequested {
            session_id: SessionId::new("session"),
            script: pending_script("execution"),
        });

    assert_eq!(
        harness
            .app
            .state
            .pending_script
            .as_ref()
            .map(|script| script.execution_id.as_str()),
        Some("execution")
    );
    assert_eq!(
        harness.app.state.script_phase,
        ScriptPhase::AwaitingApproval
    );
    assert_eq!(
        harness.app.state.script_approval_action,
        ScriptApprovalAction::Allow
    );
}

#[test]
fn foreground_context_state_changes_are_reported_to_the_user() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness
        .app
        .handle_runtime_event(Event::ContextStateChanged {
            session_id: SessionId::new("session"),
            notifications: vec![String::from(
                "src/removed.rs was automatically unpinned because it no longer exists.",
            )],
        });

    assert!(harness.app.state.status.contains("automatically unpinned"));
}

#[test]
fn background_script_approval_does_not_replace_foreground_state() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("foreground"));
    harness.app.state.pending_script = Some(pending_script("foreground-execution"));

    harness
        .app
        .handle_runtime_event(Event::ScriptApprovalRequested {
            session_id: SessionId::new("background"),
            script: pending_script("background-execution"),
        });

    assert_eq!(
        harness
            .app
            .state
            .pending_script
            .as_ref()
            .map(|script| script.execution_id.as_str()),
        Some("foreground-execution")
    );
    assert!(
        harness
            .drain_requests()
            .iter()
            .any(|request| matches!(request, RuntimeRequest::ListSessions))
    );
}

#[test]
fn confirming_script_approval_sends_execution_scoped_request() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

    harness.app.confirm_current_script_action();

    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::ApproveScript {
            session_id,
            execution_id,
        }] if session_id.as_str() == "session" && execution_id == "execution"
    ));
}

#[test]
fn rejecting_script_sends_denial_for_exact_execution() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.state.script_approval_action = ScriptApprovalAction::Reject;

    harness.app.confirm_current_script_action();

    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::DenyScript {
            session_id,
            execution_id,
        }] if session_id.as_str() == "session" && execution_id == "execution"
    ));
}

#[test]
fn approval_response_is_correlated_to_foreground_session_and_execution() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness.app.state.pending_script = Some(pending_script("current"));
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

    harness
        .app
        .handle_runtime_response(RuntimeResponse::ApproveScript {
            session_id: SessionId::new("other"),
            execution_id: String::from("current"),
            result: Ok(()),
        });
    assert_eq!(
        harness.app.state.script_phase,
        ScriptPhase::AwaitingApproval
    );

    harness
        .app
        .handle_runtime_response(RuntimeResponse::ApproveScript {
            session_id: SessionId::new("session"),
            execution_id: String::from("stale"),
            result: Ok(()),
        });
    assert_eq!(
        harness
            .app
            .state
            .pending_script
            .as_ref()
            .map(|script| script.execution_id.as_str()),
        Some("current")
    );
}

#[test]
fn approved_script_transitions_to_executing() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

    harness
        .app
        .handle_runtime_response(RuntimeResponse::ApproveScript {
            session_id: SessionId::new("session"),
            execution_id: String::from("execution"),
            result: Ok(()),
        });

    assert!(harness.app.state.pending_script.is_none());
    assert_eq!(harness.app.state.script_phase, ScriptPhase::Executing);
}

#[test]
fn pending_script_resync_clears_stale_approval_state() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: SessionId::new("session"),
            result: Box::new(Ok(session_snapshot(None))),
        });

    assert!(harness.app.state.pending_script.is_none());
    assert_eq!(harness.app.state.script_phase, ScriptPhase::Idle);
}

#[test]
fn execution_phase_counts_as_active_and_blocks_commands() {
    let mut state = AppState {
        script_phase: ScriptPhase::Executing,
        ..AppState::default()
    };
    assert!(state.runtime_is_active());
    assert!(state.turn_blocks_user_commands());

    state.script_phase = ScriptPhase::AwaitingApproval;
    assert!(!state.runtime_is_active());
    assert!(state.turn_blocks_user_commands());
}

#[test]
fn session_sync_requests_atomic_snapshot() {
    let mut harness = test_harness();
    harness
        .app
        .request_sync_for_session(&SessionId::new("session"));
    assert!(harness.drain_requests().iter().any(|request| {
        matches!(
            request,
            RuntimeRequest::GetSessionSnapshot { session_id } if session_id.as_str() == "session"
        )
    }));
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "test assertions follow fallible fixture setup"
)]
fn switching_back_installs_runtime_timer_from_session_snapshot() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    let timer: kraai_runtime::TurnTimer = serde_json::from_value(serde_json::json!({
        "started_at": std::time::SystemTime::now() - std::time::Duration::from_secs(90),
        "accumulated": std::time::Duration::from_secs(5),
        "active": true,
        "last_duration": null,
    }))?;
    harness
        .app
        .reset_chat_session(Some(SessionId::new("other")), "Session loaded");
    harness.app.handle_runtime_event(Event::TurnTimingChanged {
        session_id: SessionId::new("session"),
        timer,
    });
    assert_eq!(
        harness.app.state.turn_timer,
        kraai_runtime::TurnTimer::default()
    );
    harness
        .app
        .reset_chat_session(Some(SessionId::new("session")), "Session loaded");
    let mut snapshot = session_snapshot(None);
    snapshot.activity = SessionActivity::Streaming;
    snapshot.session.is_streaming = true;
    snapshot.turn_timer = timer;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: SessionId::new("session"),
            result: Box::new(Ok(snapshot)),
        });
    assert_eq!(harness.app.state.turn_timer, timer);
    assert!(
        harness
            .app
            .state
            .turn_timer
            .elapsed(std::time::Instant::now())
            >= Some(std::time::Duration::from_secs(95))
    );
    Ok(())
}

mod approval_overflow;
mod approval_response;
mod cost;
mod editor_review;
mod enhancements;
mod host_failure;
mod session_load;
#[expect(
    clippy::unwrap_used,
    reason = "snapshot delivery tests assert their fixtures and captured output"
)]
mod snapshot_delivery;

mod auth_response;
mod mcp_auth;

mod images;
mod model_options;
mod model_selection;
mod responsiveness;
mod session_model;
#[path = "tests/startup.rs"]
mod startup;
mod submission_recovery;
