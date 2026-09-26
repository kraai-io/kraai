use super::*;
use crate::app::StartupSync;
use kraai_runtime::RuntimeStartupState;

fn assert_startup_batch(requests: &[RuntimeRequest]) {
    assert!(matches!(
        requests,
        [
            RuntimeRequest::ListModels,
            RuntimeRequest::ListSessions,
            RuntimeRequest::ListUserInputHistory { .. },
            RuntimeRequest::GetAgentProfileCatalog,
            RuntimeRequest::FinishStartupSync,
        ]
    ));
}

#[test]
fn initial_config_and_lag_share_one_startup_batch_then_reloads_sync_again() {
    let mut harness = test_harness();
    harness.app.startup_sync = StartupSync::WaitingForRuntime;
    harness.app.handle_runtime_event(Event::ConfigLoaded);
    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::Lagged(1));
    assert!(harness.drain_requests().is_empty());
    harness
        .app
        .handle_runtime_event_bridge_message(RuntimeEventBridgeMessage::StartupComplete(Ok(
            RuntimeStartupState::Ready,
        )));
    assert_startup_batch(&harness.drain_requests());
    assert_eq!(harness.app.startup_sync, StartupSync::Synchronizing);
    harness.app.handle_runtime_event(Event::ConfigLoaded);
    assert_eq!(harness.drain_requests().len(), 4);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::StartupSyncComplete);
    assert_eq!(harness.app.startup_sync, StartupSync::Complete);
    harness.app.handle_runtime_event(Event::ConfigLoaded);
    assert_eq!(harness.drain_requests().len(), 4);
}

#[test]
fn missed_initial_config_and_failed_startup_still_finish_synchronizing() {
    for state in [
        RuntimeStartupState::Ready,
        RuntimeStartupState::Failed(String::from("initial failure")),
    ] {
        let mut harness = test_harness();
        harness.app.startup_sync = StartupSync::WaitingForRuntime;
        harness.app.handle_runtime_event_bridge_message(
            RuntimeEventBridgeMessage::StartupComplete(Ok(state)),
        );
        assert_startup_batch(&harness.drain_requests());
        harness
            .app
            .handle_runtime_response(RuntimeResponse::Models(Ok(HashMap::new())));
        assert_eq!(harness.app.startup_sync, StartupSync::Synchronizing);
        harness
            .app
            .handle_runtime_response(RuntimeResponse::AgentProfileCatalog(Err(
                kraai_runtime::RuntimeError::unavailable("fixture failure"),
            )));
        harness
            .app
            .handle_runtime_response(RuntimeResponse::StartupSyncComplete);
        assert_eq!(harness.app.startup_sync, StartupSync::Complete);
    }
}

#[test]
fn startup_message_waits_for_all_selections_and_is_submitted_once() {
    let mut harness = test_harness();
    let message = String::from("  initial question\nwith a second line  ");
    harness.app.startup_options.message = Some(message.clone());

    for selection in 0..4 {
        harness.app.maybe_send_startup_message();
        assert!(harness.drain_requests().is_empty());
        assert!(!harness.app.startup_message_sent);
        assert_eq!(harness.app.startup_options.message.as_ref(), Some(&message));
        match selection {
            0 => harness.app.state.config_loaded = true,
            1 => harness.app.state.selected_provider_id = Some(String::from("provider")),
            2 => harness.app.state.selected_model_id = Some(String::from("model")),
            _ => harness.app.state.selected_profile_id = Some(String::from("plan")),
        }
    }

    harness.app.state.is_streaming = true;
    harness.app.maybe_send_startup_message();
    assert!(harness.drain_requests().is_empty());
    assert!(!harness.app.startup_message_sent);
    harness.app.state.is_streaming = false;

    harness.app.maybe_send_startup_message();
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::CreateSession { profile_id, .. }]
            if profile_id.as_deref() == Some("plan")
    ));
    assert_eq!(
        harness
            .app
            .state
            .pending_submit
            .as_ref()
            .and_then(|submit| submit.message.as_text()),
        Some(message.as_str()),
    );
    assert!(harness.app.startup_message_sent);
    harness.app.maybe_send_startup_message();
    assert!(harness.drain_requests().is_empty());
}
