use std::collections::HashMap;
use std::io;

use crossbeam_channel::{Receiver, unbounded};
use kraai_runtime::{Event, PendingScriptInfo};
use kraai_types::{ChatRole, Message, MessageId, MessageStatus, SandboxCapability};

use super::{
    App, AppState, RuntimeRequest, RuntimeResponse, ScriptApprovalAction, ScriptPhase,
    StartupOptions, default_agent_profiles,
};

struct TestHarness {
    app: App,
    requests_rx: Receiver<RuntimeRequest>,
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

#[test]
fn built_in_profiles_expose_script_commands_and_capabilities() {
    let profiles = default_agent_profiles();
    let plan = profiles
        .iter()
        .find(|profile| profile.id == "plan")
        .expect("plan profile");
    let coding = profiles
        .iter()
        .find(|profile| profile.id == "coding")
        .expect("coding profile");

    assert_eq!(plan.commands, ["kraai-open-files", "kraai-close-files"]);
    assert!(plan.capabilities.contains(SandboxCapability::WorkspaceRead));
    assert!(
        coding
            .capabilities
            .contains(SandboxCapability::WorkspaceWrite)
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
        .handle_runtime_response(RuntimeResponse::PendingScript {
            session_id: String::from("session"),
            result: Ok(None),
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
            role: ChatRole::ToolCallResult,
            content: String::from("<tool_call_result status=\"completed\" />"),
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
fn session_sync_requests_pending_script_state() {
    let mut harness = test_harness();
    harness.app.request_sync_for_session("session");
    assert!(harness.drain_requests().iter().any(|request| {
        matches!(
            request,
            RuntimeRequest::GetPendingScript { session_id } if session_id == "session"
        )
    }));
}
