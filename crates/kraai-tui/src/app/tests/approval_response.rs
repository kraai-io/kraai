use super::{
    Event, RuntimeEvent, RuntimeEventBridgeMessage, RuntimeResponse, ScriptApprovalAction,
    ScriptPhase, pending_script, test_harness,
};
use crossbeam_channel::unbounded;

fn success_response(action: ScriptApprovalAction, execution_id: &str) -> RuntimeResponse {
    match action {
        ScriptApprovalAction::Allow => RuntimeResponse::ApproveScript {
            session_id: String::from("session"),
            execution_id: execution_id.to_string(),
            result: Ok(()),
        },
        ScriptApprovalAction::Reject => RuntimeResponse::DenyScript {
            session_id: String::from("session"),
            execution_id: execution_id.to_string(),
            result: Ok(()),
        },
    }
}

#[test]
fn delayed_decision_reply_preserves_a_newer_approval_prompt() {
    for action in [ScriptApprovalAction::Allow, ScriptApprovalAction::Reject] {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("session"));
        harness.app.state.pending_script = Some(pending_script("previous"));
        harness.app.state.script_phase = ScriptPhase::AwaitingApproval;
        let (event_tx, event_rx) = unbounded();
        let (response_tx, response_rx) = unbounded();
        harness.app.event_rx = event_rx;
        harness.app.runtime_rx = response_rx;
        assert!(
            response_tx
                .send(success_response(action, "previous"))
                .is_ok()
        );
        let events = [
            Event::ScriptResultReady {
                session_id: String::from("session"),
                execution_id: String::from("previous"),
                status: String::from("completed"),
            },
            Event::StreamStart {
                session_id: String::from("session"),
                message_id: String::from("continuation"),
            },
            Event::StreamComplete {
                session_id: String::from("session"),
                message_id: String::from("continuation"),
            },
            Event::ScriptApprovalRequested {
                session_id: String::from("session"),
                script: pending_script("current"),
            },
        ];
        for (index, event) in events.into_iter().enumerate() {
            assert!(
                event_tx
                    .send(RuntimeEventBridgeMessage::Event(RuntimeEvent {
                        sequence: index as u64 + 1,
                        event,
                    }))
                    .is_ok()
            );
        }

        assert!(harness.app.process_events());
        assert_eq!(
            harness.app.state.script_phase,
            ScriptPhase::AwaitingApproval
        );
        assert_eq!(
            harness
                .app
                .state
                .pending_script
                .as_ref()
                .map(|script| script.execution_id.as_str()),
            Some("current"),
        );
        assert_eq!(
            harness.app.state.status,
            "Script capability approval required"
        );
        harness.app.confirm_current_script_action();
        assert!(harness.drain_requests().iter().any(|request| matches!(
            request,
            super::RuntimeRequest::ApproveScript { execution_id, .. }
                if execution_id == "current"
        )));
    }
}

#[test]
fn delayed_decision_reply_does_not_restart_a_finished_script() {
    for action in [ScriptApprovalAction::Allow, ScriptApprovalAction::Reject] {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("session"));
        harness.app.state.pending_script = Some(pending_script("previous"));
        harness.app.state.script_phase = ScriptPhase::AwaitingApproval;
        harness.app.handle_runtime_event(Event::ScriptResultReady {
            session_id: String::from("session"),
            execution_id: String::from("previous"),
            status: String::from("completed"),
        });
        let status = harness.app.state.status.clone();

        harness
            .app
            .handle_runtime_response(success_response(action, "previous"));

        assert_eq!(harness.app.state.script_phase, ScriptPhase::Idle);
        assert!(harness.app.state.pending_script.is_none());
        assert_eq!(harness.app.state.status, status);
    }
}

#[test]
fn current_decision_reply_keeps_existing_success_status() {
    for (action, status) in [
        (
            ScriptApprovalAction::Allow,
            "Executing approved Nushell script",
        ),
        (
            ScriptApprovalAction::Reject,
            "Script capability escalation denied",
        ),
    ] {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(String::from("session"));
        harness.app.state.pending_script = Some(pending_script("current"));
        harness.app.state.script_phase = ScriptPhase::AwaitingApproval;

        harness
            .app
            .handle_runtime_response(success_response(action, "current"));

        assert_eq!(harness.app.state.script_phase, ScriptPhase::Executing);
        assert!(harness.app.state.pending_script.is_none());
        assert_eq!(harness.app.state.status, status);
    }
}
