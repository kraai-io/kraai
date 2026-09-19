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
