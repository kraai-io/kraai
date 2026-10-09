use super::{
    Event, RuntimeEvent, RuntimeEventBridgeMessage, RuntimeResponse, ScriptApprovalAction,
    ScriptPhase, pending_script, test_harness,
};
use crossbeam_channel::unbounded;
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

fn rendered(harness: &super::TestHarness) -> String {
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    (&harness.app.state).render(area, &mut buffer);
    buffer.content().iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn submitting_a_decision_hides_the_prompt_before_the_reply() {
    for action in [ScriptApprovalAction::Allow, ScriptApprovalAction::Reject] {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some("session".into());
        harness.app.state.pending_script = Some(pending_script("execution"));
        harness.app.enter_script_decision_phase();
        harness.app.state.script_approval_action = action;
        harness.app.state.approval_expanded = true;
        assert!(rendered(&harness).contains("Permission required"));

        harness.app.handle_key_event(crate::app::KeyEvent::new(
            crate::app::KeyCode::Enter,
            crate::app::KeyModifiers::NONE,
        ));

        assert!(!rendered(&harness).contains("Permission required"));
        assert!(rendered(&harness).contains("Submitting decision"));
        assert!(harness.app.state.composer_cursor_enabled());
        assert!(harness.app.state.runtime_is_active());
        harness.app.confirm_current_script_action();
        let requests = harness.drain_requests();
        assert!(matches!(
            (action, requests.as_slice()),
            (
                ScriptApprovalAction::Allow,
                [super::RuntimeRequest::ApproveScript {
                    session_id,
                    execution_id,
                }]
            ) | (
                ScriptApprovalAction::Reject,
                [super::RuntimeRequest::DenyScript {
                    session_id,
                    execution_id,
                }]
            ) if session_id == "session" && execution_id == "execution"
        ));

        harness
            .app
            .reset_chat_session(Some("other".into()), "Switched session");
        harness
            .app
            .reset_chat_session(Some("session".into()), "Switched session");
        harness
            .app
            .handle_runtime_response(RuntimeResponse::SessionSnapshot {
                model_save_id: 0,
                session_id: "session".into(),
                result: Box::new(Ok(super::session_snapshot(Some(pending_script(
                    "execution",
                ))))),
            });
        assert!(!rendered(&harness).contains("Permission required"));

        harness
            .app
            .handle_runtime_response(decision_response(action, "execution", Ok(())));
        assert!(!harness.app.state.has_local_script_approval());
        assert!(harness.app.state.submitted_script_decisions.is_empty());
        assert!(harness.app.state.pending_script.is_none());
    }
}

#[test]
fn failed_decision_restores_the_prompt_and_allows_retry() {
    for action in [ScriptApprovalAction::Allow, ScriptApprovalAction::Reject] {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some("session".into());
        harness.app.state.pending_script = Some(pending_script("execution"));
        harness.app.enter_script_decision_phase();
        harness.app.state.script_approval_action = action;
        harness.app.confirm_current_script_action();
        harness.drain_requests();

        harness.app.handle_runtime_response(decision_response(
            action,
            "execution",
            Err(kraai_runtime::RuntimeError::conflict("Decision failed")),
        ));

        assert!(rendered(&harness).contains("Permission required"));
        assert!(harness.app.state.last_error.is_some());
        assert!(harness.app.state.submitted_script_decisions.is_empty());
        harness.app.confirm_current_script_action();
        assert!(!harness.app.state.has_local_script_approval());
        assert_eq!(harness.drain_requests().len(), 1);
    }
}

#[test]
fn undelivered_decision_is_not_marked_as_submitted() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some("session".into());
    harness.app.state.pending_script = Some(pending_script("execution"));
    harness.app.enter_script_decision_phase();
    let (runtime_tx, runtime_rx) = unbounded();
    harness.app.runtime_tx = runtime_tx;
    drop(runtime_rx);

    harness.app.confirm_current_script_action();

    assert!(harness.app.state.submitted_script_decisions.is_empty());
    assert_eq!(harness.app.state.status, "Runtime bridge disconnected");
}

fn decision_response(
    action: ScriptApprovalAction,
    execution_id: &str,
    result: Result<(), kraai_runtime::RuntimeError>,
) -> RuntimeResponse {
    match action {
        ScriptApprovalAction::Allow => RuntimeResponse::ApproveScript {
            session_id: String::from("session"),
            execution_id: execution_id.to_string(),
            result,
        },
        ScriptApprovalAction::Reject => RuntimeResponse::DenyScript {
            session_id: String::from("session"),
            execution_id: execution_id.to_string(),
            result,
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
        harness.app.state.script_approval_action = action;
        harness.app.confirm_current_script_action();
        let (event_tx, event_rx) = unbounded();
        let (response_tx, response_rx) = unbounded();
        harness.app.event_rx = event_rx;
        harness.app.runtime_rx = response_rx;
        assert!(
            response_tx
                .send(decision_response(action, "previous", Ok(())))
                .is_ok()
        );
        let events = [
            Event::ScriptResultReady {
                session_id: String::from("session"),
                execution_id: String::from("previous"),
                call_id: String::from("call"),
                output: Default::default(),
                outcome: kraai_types::ScriptExecutionOutcome {
                    status: kraai_types::ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                },
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

        assert!(matches!(harness.app.process_events(|| Ok(false)), Ok(true)));
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
        harness.app.state.script_approval_action = action;
        harness.app.confirm_current_script_action();
        harness.app.handle_runtime_event(Event::ScriptResultReady {
            session_id: String::from("session"),
            execution_id: String::from("previous"),
            call_id: String::from("call"),
            output: Default::default(),
            outcome: kraai_types::ScriptExecutionOutcome {
                status: kraai_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
        });
        let status = harness.app.state.status.clone();

        harness
            .app
            .handle_runtime_response(decision_response(action, "previous", Ok(())));

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
            .handle_runtime_response(decision_response(action, "current", Ok(())));

        assert_eq!(harness.app.state.script_phase, ScriptPhase::Executing);
        assert!(harness.app.state.pending_script.is_none());
        assert_eq!(harness.app.state.status, status);
    }
}

#[test]
fn foreign_approval_keeps_the_composer_available_and_recovers_rejected_input() {
    use crate::app::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    let mut harness = test_harness();
    harness.app.state.current_session_id = Some("session".into());
    harness.app.state.config_loaded = true;
    harness.app.state.selected_provider_id = Some("provider".into());
    harness.app.state.selected_model_id = Some("model".into());
    let mut snapshot = super::session_snapshot(None);
    snapshot.activity = kraai_runtime::SessionActivity::AwaitingApproval;
    snapshot.session.waiting_for_approval = true;
    snapshot.session.profile_locked = true;
    snapshot.session.is_running = true;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: "session".into(),
            result: Box::new(Ok(snapshot)),
        });
    assert_eq!(
        harness.app.state.script_phase,
        ScriptPhase::AwaitingApproval
    );
    assert!(harness.app.state.pending_script.is_none());
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    harness.app.handle_paste("ello".into());
    assert_eq!(harness.app.state.input, "hello");

    harness.app.state.chat_render_cache.borrow_mut().total_lines = 100;
    harness.app.state.chat_viewport_height = 20;
    harness.app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(harness.app.state.scroll, 77);
    assert_eq!(harness.app.state.approval_scroll.get(), 0);

    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    (&harness.app.state).render(area, &mut buffer);
    assert!(buffer.content().chunks(80).any(|row| {
        row.iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .contains("hello")
    }));

    harness.drain_requests();
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        harness.requests_rx.try_recv(),
        Ok(super::RuntimeRequest::SendMessage { session_id, message, .. })
            if session_id == "session" && message.as_text() == Some("hello")
    ));
    assert!(harness.app.state.input.is_empty());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "session".into(),
            result: Err(kraai_runtime::RuntimeError::conflict(
                "Owned by another runtime",
            )),
        });
    assert_eq!(harness.app.state.input, "hello");
    assert!(harness.app.state.pending_messages.is_empty());
}

#[test]
fn foreign_approval_keeps_the_execution_view_open() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some("session".into());
    harness.app.state.mode = crate::app::UiMode::Executions;
    let mut snapshot = super::session_snapshot(None);
    snapshot.activity = kraai_runtime::SessionActivity::AwaitingApproval;
    snapshot.session.waiting_for_approval = true;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: "session".into(),
            result: Box::new(Ok(snapshot)),
        });
    assert_eq!(harness.app.state.mode, crate::app::UiMode::Executions);
}
