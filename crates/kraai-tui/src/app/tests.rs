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

use std::io;

use crossbeam_channel::{Receiver, unbounded};
use kraai_runtime::{
    AgentProfilesState, Event, PendingScriptInfo, RuntimeEvent, Session, SessionActivity,
    SessionSnapshot,
};
use kraai_types::{Message, MessageId, MessageStatus};

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
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.state.script_phase = ScriptPhase::Executing;

    harness.app.handle_key_event(super::KeyEvent::new(
        super::KeyCode::Esc,
        super::KeyModifiers::NONE,
    ));

    assert!(matches!(
        harness.requests_rx.try_recv(),
        Ok(RuntimeRequest::CancelStream { session_id }) if session_id == "session"
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
            ci_output: Box::new(io::sink()),
            ci_output_needs_newline: false,
            ci_turn_completion_pending: false,
            ci_metrics_history_pending: false,
            ci_metrics_context_pending: false,
            startup_options: StartupOptions::default(),
            startup_message_sent: false,
            ci_error: None,
            stream_event_content: HashMap::new(),
            state: AppState::default(),
            last_stream_history_request: None,
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
        event_sequence,
        session: Session {
            id: String::from("session"),
            tip_id: None,
            workspace_dir: String::from("/workspace"),
            created_at: 0,
            updated_at: 0,
            title: None,
            selected_profile_id: Some(String::from("plan")),
            profile_locked: pending_script.is_some(),
            waiting_for_approval: pending_script.is_some(),
            is_streaming: false,
            is_running: pending_script.is_some(),
        },
        history: std::collections::BTreeMap::new(),
        context_usage: None,
        pending_script,
        profiles: AgentProfilesState {
            profiles: Vec::new(),
            warnings: Vec::new(),
            selected_profile_id: Some(String::from("plan")),
            profile_locked: false,
        },
        activity,
        queued_messages: 0,
    }
}

#[test]
fn snapshot_watermarks_only_suppress_covered_events_for_the_same_session() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("other-session"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(session_snapshot_at(10, None))),
        });

    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Event(RuntimeEvent {
            sequence: 9,
            event: Event::ContextStateChanged {
                session_id: String::from("other-session"),
                notifications: vec![String::from("other session event was delivered")],
            },
        }));
    assert_eq!(
        harness.app.state.status,
        "other session event was delivered"
    );

    harness.app.state.current_session_id = Some(String::from("session"));
    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Event(RuntimeEvent {
            sequence: 10,
            event: Event::ContextStateChanged {
                session_id: String::from("session"),
                notifications: vec![String::from("covered event should be ignored")],
            },
        }));
    assert_ne!(harness.app.state.status, "covered event should be ignored");
}

#[test]
fn snapshot_is_rejected_only_when_a_newer_event_for_its_session_was_applied() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Event(RuntimeEvent {
            sequence: 11,
            event: Event::ContextStateChanged {
                session_id: String::from("session"),
                notifications: vec![String::from("newer session state")],
            },
        }));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(session_snapshot_at(10, None))),
        });

    assert_eq!(harness.app.state.status, "newer session state");
    assert!(
        !harness
            .app
            .session_snapshot_sequences
            .contains_key("session")
    );
}

#[test]
fn script_approval_event_enters_single_script_decision_state() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness
        .app
        .handle_runtime_event(Event::ScriptApprovalRequested {
            session_id: String::from("session"),
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
fn ci_script_approval_fails_closed() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.current_session_id = Some(String::from("session"));

    harness
        .app
        .handle_runtime_event(Event::ScriptApprovalRequested {
            session_id: String::from("session"),
            script: pending_script("execution"),
        });

    assert_eq!(
        harness.app.ci_error.as_deref(),
        Some("CI mode cannot answer a script capability escalation prompt")
    );
    assert!(harness.drain_requests().is_empty());
}

#[test]
fn foreground_context_state_changes_are_reported_to_the_user() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness
        .app
        .handle_runtime_event(Event::ContextStateChanged {
            session_id: String::from("session"),
            notifications: vec![String::from(
                "src/removed.rs was automatically unpinned because it no longer exists.",
            )],
        });

    assert!(harness.app.state.status.contains("automatically unpinned"));
}

#[test]
fn background_script_approval_does_not_replace_foreground_state() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("foreground"));
    harness.app.state.pending_script = Some(pending_script("foreground-execution"));

    harness
        .app
        .handle_runtime_event(Event::ScriptApprovalRequested {
            session_id: String::from("background"),
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
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

    harness.app.confirm_current_script_action();

    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::ApproveScript {
            session_id,
            execution_id,
        }] if session_id == "session" && execution_id == "execution"
    ));
}

#[test]
fn rejecting_script_sends_denial_for_exact_execution() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.state.script_approval_action = ScriptApprovalAction::Reject;

    harness.app.confirm_current_script_action();

    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::DenyScript {
            session_id,
            execution_id,
        }] if session_id == "session" && execution_id == "execution"
    ));
}

#[test]
fn approval_response_is_correlated_to_foreground_session_and_execution() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.state.pending_script = Some(pending_script("current"));
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

    harness
        .app
        .handle_runtime_response(RuntimeResponse::ApproveScript {
            session_id: String::from("other"),
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
            session_id: String::from("session"),
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
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

    harness
        .app
        .handle_runtime_response(RuntimeResponse::ApproveScript {
            session_id: String::from("session"),
            execution_id: String::from("execution"),
            result: Ok(()),
        });

    assert!(harness.app.state.pending_script.is_none());
    assert_eq!(harness.app.state.script_phase, ScriptPhase::Executing);
}

#[test]
fn pending_script_resync_clears_stale_approval_state() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
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
fn evaluation_metrics_count_script_results() {
    let mut harness = test_harness();
    harness.app.state.chat_history.insert(
        MessageId::new("result"),
        Message {
            id: MessageId::new("result"),
            parent_id: None,
            content: kraai_types::ConversationItem::ScriptResult {
                call_id: kraai_types::ToolCallId::new("call-1"),
                output: String::from("<tool_call_result status=\"completed\" />"),
            },
            status: MessageStatus::Complete,
            agent_profile_id: Some(String::from("coding")),
            generation: None,
        },
    );

    let metrics = harness.app.evaluation_metrics();
    assert_eq!(metrics["script_executions"], 1);
    assert!(metrics.get("tool_calls").is_none());
}

#[test]
fn session_sync_requests_atomic_snapshot() {
    let mut harness = test_harness();
    harness.app.request_sync_for_session("session");
    assert!(harness.drain_requests().iter().any(|request| {
        matches!(
            request,
            RuntimeRequest::GetSessionSnapshot { session_id } if session_id == "session"
        )
    }));
}
