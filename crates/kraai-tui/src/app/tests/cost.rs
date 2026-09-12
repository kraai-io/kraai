use super::*;

fn pending_request(id: &str) -> kraai_types::RequestUsage {
    kraai_types::RequestUsage {
        message_id: MessageId::new(id),
        provider_id: kraai_types::ProviderId::new("provider"),
        model_id: kraai_types::ModelId::new("model"),
        started_at: 101,
        subscription: false,
        unpriced_attempts: 0,
        usage: None,
    }
}

#[test]
fn lag_recovery_restores_costs_from_background_sessions() {
    let mut harness = test_harness();
    harness.app.state.launched_at = 100;
    harness.app.reset_chat_session(Some("session".into()), "");
    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Lagged(3));
    harness.drain_requests();

    let mut background = session_snapshot_at(10, None);
    background.session.id = "background".into();
    let request = pending_request("missed-request");
    background
        .requests
        .insert(request.message_id.clone(), request.clone());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Sessions(Ok(vec![
            session_snapshot(None).session,
            background.session.clone(),
        ])));
    assert!(harness.drain_requests().iter().any(|request| matches!(
        request,
        RuntimeRequest::GetSessionSnapshot { session_id } if session_id == "background"
    )));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: "background".into(),
            result: Box::new(Ok(background)),
        });
    assert_eq!(
        harness.app.state.launch_requests.get(&request.message_id),
        Some(&request)
    );
    assert_eq!(harness.app.state.session_cost.unknown, 0);
    assert!(
        harness
            .app
            .exit_cost_summary()
            .iter()
            .any(|line| line.contains("total: unknown (1 request)"))
    );
    harness
        .app
        .reset_chat_session(Some("background".into()), "");
    assert_eq!(harness.app.state.session_cost.unknown, 1);
}

#[test]
fn quitting_during_lag_recovery_marks_costs_incomplete() {
    for ci in [false, true] {
        let mut harness = test_harness();
        harness.app.startup_options.ci = ci;
        harness.app.reset_chat_session(Some("session".into()), "");
        harness
            .app
            .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Lagged(3));
        if ci {
            harness.app.handle_runtime_event(Event::StreamError {
                session_id: "session".into(),
                message_id: "missed-request".into(),
                error: "connection lost".into(),
            });
        } else {
            harness.app.handle_ctrl_c();
            harness.app.handle_ctrl_c();
        }
        assert!(harness.app.state.exit);
        assert_eq!(
            harness.app.evaluation_metrics()["request_costs_complete"],
            false
        );
        assert!(
            harness
                .app
                .exit_token_usage_summary()
                .is_some_and(|summary| summary.contains("incomplete: runtime events missed"))
        );
    }
}

#[test]
fn successful_snapshots_clear_incomplete_costs_after_lag() {
    let mut harness = test_harness();
    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Lagged(3));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Sessions(Ok(vec![
            session_snapshot(None).session,
        ])));
    assert!(harness.app.costs_incomplete());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: "session".into(),
            result: Box::new(Ok(session_snapshot_at(10, None))),
        });
    assert!(!harness.app.costs_incomplete());
    assert_eq!(
        harness.app.evaluation_metrics()["request_costs_complete"],
        true
    );
}

#[test]
fn usage_events_include_inflight_and_background_requests_before_exit() {
    let mut harness = test_harness();
    harness.app.state.launched_at = 100;
    harness.app.reset_chat_session(Some("session".into()), "");
    for session in ["session", "background"] {
        harness
            .app
            .handle_runtime_event(Event::RequestUsageUpdated {
                session_id: session.into(),
                request: Box::new(pending_request(session)),
            });
    }
    assert_eq!(harness.app.state.session_cost.unknown, 1);
    assert_eq!(harness.app.state.launch_requests.len(), 2);
    harness.app.handle_ctrl_c();
    harness.app.handle_ctrl_c();
    assert!(harness.app.state.exit);
    assert!(
        harness
            .app
            .exit_token_usage_summary()
            .is_some_and(|summary| summary.contains("total: unknown (2 requests)"))
    );
    harness
        .app
        .reset_chat_session(Some("background".into()), "");
    assert_eq!(harness.app.state.session_cost.unknown, 1);
}

#[test]
fn ci_failure_retains_cost_events_without_waiting_for_a_snapshot() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.launched_at = 100;
    harness.app.reset_chat_session(Some("session".into()), "");
    let request = pending_request("request");
    harness
        .app
        .handle_runtime_event(Event::RequestUsageUpdated {
            session_id: "session".into(),
            request: Box::new(request.clone()),
        });
    harness.app.handle_runtime_event(Event::StreamError {
        session_id: "session".into(),
        message_id: "request".into(),
        error: "connection lost".into(),
    });
    assert!(harness.app.state.exit);
    assert_eq!(
        harness.app.state.launch_requests.get(&request.message_id),
        Some(&request)
    );
    assert_eq!(
        harness.app.evaluation_metrics()["request_costs"]["request"]["usage"],
        serde_json::Value::Null
    );
}

#[test]
fn stale_and_duplicate_snapshots_do_not_erase_live_costs() {
    let mut harness = test_harness();
    harness.app.state.launched_at = 100;
    harness.app.reset_chat_session(Some("session".into()), "");
    let pending = pending_request("request");
    let mut completed = pending.clone();
    completed.unpriced_attempts = 1;
    completed.usage = Some(kraai_types::TokenUsage {
        cost: Some(kraai_types::RequestCost {
            amount: kraai_types::Usd(12_300_000),
            source: "openrouter".into(),
            rates: None,
            priced_at: 1,
            upstream: None,
        }),
        ..Default::default()
    });
    harness
        .app
        .handle_runtime_event(Event::RequestUsageUpdated {
            session_id: "session".into(),
            request: Box::new(completed.clone()),
        });
    for request in [completed.clone(), pending] {
        harness.app.update_costs(
            "session",
            std::collections::BTreeMap::from([(request.message_id.clone(), request)]),
        );
    }
    harness.app.update_costs("session", Default::default());
    assert_eq!(harness.app.state.launch_requests.len(), 1);
    assert_eq!(
        harness.app.state.launch_requests.get(&completed.message_id),
        Some(&completed)
    );
    assert_eq!(
        harness.app.state.session_cost.amount,
        kraai_types::Usd(12_300_000)
    );
    assert_eq!(harness.app.state.session_cost.unknown, 1);
}

#[test]
fn costs_replace_unknown_records_without_double_counting_and_restore_session_totals() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some("session".into());
    harness.app.state.launched_at = 100;
    let mut request = kraai_types::RequestUsage {
        message_id: MessageId::new("request"),
        provider_id: kraai_types::ProviderId::new("provider"),
        model_id: kraai_types::ModelId::new("model"),
        started_at: 101,
        subscription: false,
        unpriced_attempts: 0,
        usage: None,
    };
    let records = |request: &kraai_types::RequestUsage| {
        std::collections::BTreeMap::from([(request.message_id.clone(), request.clone())])
    };
    harness.app.update_costs("session", records(&request));
    assert_eq!(harness.app.state.session_cost.unknown, 1);
    request.usage = Some(kraai_types::TokenUsage {
        cost: Some(kraai_types::RequestCost {
            amount: kraai_types::Usd(12_300_000),
            source: "openrouter".into(),
            rates: None,
            priced_at: 1,
            upstream: None,
        }),
        ..Default::default()
    });
    harness.app.update_costs("session", records(&request));
    harness.app.update_costs("session", records(&request));
    assert_eq!(harness.app.state.session_cost.to_string(), "$0.0123");
    assert_eq!(harness.app.state.launch_requests.len(), 1);
    assert!(
        harness
            .app
            .exit_token_usage_summary()
            .is_some_and(|summary| summary.contains("total: $0.0123"))
    );
    request.message_id = MessageId::new("historical");
    request.started_at = 1;
    harness.app.update_costs("other", records(&request));
    assert_eq!(harness.app.state.launch_requests.len(), 1);
    harness.app.state.current_session_id = Some("other".into());
    harness.app.update_costs("other", records(&request));
    assert_eq!(harness.app.state.session_cost.to_string(), "$0.0123");
}
