use std::future::{poll_fn, ready};
use std::pin::Pin;
use std::task::Poll;

use kraai_runtime::{Event, RuntimeStartupState};

use super::*;

async fn poll_pending(future: Pin<&mut impl Future<Output = ()>>) {
    let mut future = future;
    poll_fn(|context| {
        assert!(future.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
}

fn config_event(sequence: u64) -> RuntimeEvent {
    RuntimeEvent {
        sequence,
        event: Event::ConfigLoaded,
    }
}

#[tokio::test]
async fn buffered_config_events_precede_startup_and_later_reloads_are_forwarded() {
    let (updates, receiver) = broadcast::channel(8);
    let (sender, messages) = unbounded();
    updates.send(config_event(1)).unwrap();
    let mut forwarding = Box::pin(forward_runtime_events(
        receiver,
        sender,
        ready(Ok(RuntimeStartupState::ready())),
    ));
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv().unwrap(),
        RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence: 1, .. })
    ));
    assert!(matches!(
        messages.try_recv().unwrap(),
        RuntimeEventBridgeMessage::StartupComplete(Ok(RuntimeStartupState::Ready { .. }))
    ));
    updates.send(config_event(2)).unwrap();
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv().unwrap(),
        RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence: 2, .. })
    ));
    assert!(messages.is_empty());
    drop(updates);
    forwarding.await;
}

#[tokio::test]
async fn retained_startup_result_covers_late_subscribers_and_failure() {
    for state in [
        RuntimeStartupState::ready(),
        RuntimeStartupState::Failed(String::from("initial failure")),
    ] {
        let (updates, receiver) = broadcast::channel(8);
        drop(receiver);
        let _ = updates.send(config_event(1));
        let (sender, messages) = unbounded();
        let mut forwarding = Box::pin(forward_runtime_events(
            updates.subscribe(),
            sender,
            ready(Ok(state.clone())),
        ));
        poll_pending(forwarding.as_mut()).await;
        assert!(matches!(
            messages.try_recv().unwrap(),
            RuntimeEventBridgeMessage::StartupComplete(Ok(actual)) if actual == state
        ));
        assert!(messages.is_empty());
        drop(updates);
        forwarding.await;
    }
}

#[tokio::test]
async fn startup_lag_is_reported_before_the_completion_marker() {
    let (updates, receiver) = broadcast::channel(2);
    let (sender, messages) = unbounded();
    for sequence in 1..=3 {
        updates.send(config_event(sequence)).unwrap();
    }
    let mut forwarding = Box::pin(forward_runtime_events(
        receiver,
        sender,
        ready(Ok(RuntimeStartupState::ready())),
    ));
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv().unwrap(),
        RuntimeEventBridgeMessage::Lagged
    ));
    for sequence in 2..=3 {
        assert!(matches!(
            messages.try_recv().unwrap(),
            RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence: actual, .. }) if actual == sequence
        ));
    }
    assert!(matches!(
        messages.try_recv().unwrap(),
        RuntimeEventBridgeMessage::StartupComplete(Ok(RuntimeStartupState::Ready { .. }))
    ));
    drop(updates);
    forwarding.await;
}

#[tokio::test]
async fn closed_events_cannot_leave_startup_waiting_forever() {
    let (updates, receiver) = broadcast::channel(2);
    drop(updates);
    let (sender, messages) = unbounded();
    forward_runtime_events(receiver, sender, std::future::pending()).await;
    assert!(matches!(
        messages.try_recv().unwrap(),
        RuntimeEventBridgeMessage::StartupComplete(Err(_))
    ));
}

#[tokio::test]
async fn runtime_events_remain_live_while_startup_is_pending() {
    let (updates, receiver) = broadcast::channel(8);
    let (sender, messages) = unbounded();
    let (started, startup) = tokio::sync::oneshot::channel();
    let mut forwarding = Box::pin(forward_runtime_events(receiver, sender, async move {
        startup
            .await
            .map_err(|error| RuntimeError::unavailable(error.to_string()))
    }));
    poll_pending(forwarding.as_mut()).await;
    assert!(messages.is_empty());
    updates.send(config_event(1)).unwrap();
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv().unwrap(),
        RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence: 1, .. })
    ));
    assert!(messages.is_empty());
    assert!(started.send(RuntimeStartupState::ready()).is_ok());
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv().unwrap(),
        RuntimeEventBridgeMessage::StartupComplete(Ok(RuntimeStartupState::Ready { .. }))
    ));
    drop(updates);
    forwarding.await;
}

#[test]
fn startup_drain_finishes_even_when_every_forwarded_event_is_replaced() {
    let (updates, mut receiver) = broadcast::channel(2);
    updates.send(config_event(1)).unwrap();
    let mut forwarded = 0;
    let drained = drain_startup_events(&mut receiver, |event| {
        forwarded += 1;
        assert!(matches!(event, Ok(RuntimeEvent { sequence, .. }) if sequence == forwarded));
        assert!(updates.send(config_event(forwarded + 1)).is_ok());
        forwarded < 16
    });
    assert!(drained);
    assert_eq!(forwarded, 1);
    assert_eq!(receiver.try_recv().unwrap().sequence, 2);
}

#[test]
fn startup_drain_counts_overwritten_messages_against_the_original_backlog() {
    let (updates, mut receiver) = broadcast::channel(2);
    for sequence in 1..=3 {
        updates.send(config_event(sequence)).unwrap();
    }
    let mut forwarded = Vec::new();
    let mut overwritten = 0;
    assert!(drain_startup_events(&mut receiver, |event| {
        match event {
            Ok(event) => forwarded.push(event.sequence),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                assert_eq!(skipped, 1);
                overwritten += skipped;
                if overwritten == 1 {
                    assert!(updates.send(config_event(4)).is_ok());
                }
            }
            Err(broadcast::error::RecvError::Closed) => return false,
        }
        true
    }));
    assert_eq!(overwritten, 2);
    assert_eq!(forwarded, vec![3]);
    assert_eq!(receiver.try_recv().unwrap().sequence, 4);
}

#[tokio::test]
async fn request_bridge_finishes_startup_after_empty_and_failed_responses() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("providers.toml");
    std::fs::write(&config, "").unwrap();
    let runtime = kraai_runtime::RuntimeBuilder::new(kraai_providers::DefaultProviders)
        .storage_root(root.path().to_path_buf())
        .provider_config_path(config)
        .build();
    let (requests, responses) = spawn_runtime_bridge(runtime.clone());
    for request in [
        RuntimeRequest::ListModels,
        RuntimeRequest::ListSessions,
        RuntimeRequest::ListUserInputHistory { limit: 100 },
        RuntimeRequest::GetAgentProfileCatalog,
        RuntimeRequest::GetSessionSnapshot {
            session_id: SessionId::new("missing"),
        },
        RuntimeRequest::FinishStartupSync,
    ] {
        assert!(requests.send(request).is_ok());
    }
    let timeout = kraai_test_support::HANG_TIMEOUT;
    assert!(
        matches!(responses.recv_timeout(timeout).unwrap(), RuntimeResponse::Models(Ok(models)) if models.is_empty())
    );
    assert!(
        matches!(responses.recv_timeout(timeout).unwrap(), RuntimeResponse::Sessions(Ok(sessions)) if sessions.is_empty())
    );
    assert!(
        matches!(responses.recv_timeout(timeout).unwrap(), RuntimeResponse::UserInputHistory(Ok(history)) if history.is_empty())
    );
    assert!(matches!(
        responses.recv_timeout(timeout).unwrap(),
        RuntimeResponse::AgentProfileCatalog(_)
    ));
    assert!(
        matches!(responses.recv_timeout(timeout).unwrap(), RuntimeResponse::SessionSnapshot { result, .. } if result.is_err())
    );
    assert!(matches!(
        responses.recv_timeout(timeout).unwrap(),
        RuntimeResponse::StartupSyncComplete
    ));
    assert!(responses.is_empty());
    drop(requests);
    kraai_test_support::hang_guard(runtime.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn failed_executor_preserves_response_ids_and_startup_marker() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("providers.toml");
    std::fs::write(&config, "").unwrap();
    let runtime = kraai_runtime::RuntimeBuilder::new(kraai_providers::DefaultProviders)
        .storage_root(root.path().to_path_buf())
        .provider_config_path(config)
        .build();
    let failure = RuntimeError::unavailable("failed to create tokio runtime: injected");
    let mut bridge = RequestBridge {
        runtime: runtime.clone(),
        executor: Err(failure.clone()),
        model_save_ids: Default::default(),
    };
    let attempted = std::cell::Cell::new(false);
    let result = bridge.execute(|_| {
        attempted.set(true);
        ready(Ok(()))
    });
    assert!(!attempted.get());
    assert_eq!(result, Err(failure.clone()));
    assert!(matches!(
        bridge.dispatch(RuntimeRequest::SetSessionProfile {
            session_id: SessionId::new("session"),
            profile_id: String::new(),
        }),
        RuntimeResponse::SetSessionProfile { session_id, profile_id, result }
            if session_id == SessionId::new("session") && profile_id.is_empty() && result == Err(failure.clone())
    ));
    assert!(matches!(
        bridge.dispatch(RuntimeRequest::GetSessionSnapshot { session_id: SessionId::new("missing") }),
        RuntimeResponse::SessionSnapshot { session_id, result, .. }
            if session_id == SessionId::new("missing") && result.as_ref().as_ref().err() == Some(&failure)
    ));
    assert!(matches!(
        bridge.dispatch(RuntimeRequest::ApproveScript {
            session_id: SessionId::new("session"), execution_id: "invalid/execution".into(),
        }),
        RuntimeResponse::ApproveScript { session_id, execution_id, result }
            if session_id == SessionId::new("session") && execution_id == "invalid/execution" && result == Err(failure.clone())
    ));
    assert!(matches!(
        bridge.dispatch(RuntimeRequest::GetProviderAuthStatuses),
        RuntimeResponse::ProviderAuthStatuses(Err(error)) if error == failure
    ));
    assert!(matches!(
        bridge.dispatch(RuntimeRequest::LoadSession {
            load_id: 42,
            session_id: SessionId::new("session"),
        }),
        RuntimeResponse::LoadSession { load_id: 42, session_id, result }
            if session_id == SessionId::new("session") && result == Err(failure.clone())
    ));
    assert!(matches!(
        bridge.dispatch(RuntimeRequest::FinishStartupSync),
        RuntimeResponse::StartupSyncComplete
    ));
    runtime.shutdown().await.unwrap();
}

#[test]
fn turn_controls_and_model_fetches_do_not_queue_behind_refreshes() {
    let (requests, receiver) = unbounded();
    let (queries, queued) = unbounded();
    let (model_lists, fetches) = unbounded();
    let (commands, controls) = unbounded();
    let session_id = || SessionId::new("session");
    for request in [
        RuntimeRequest::ListSessions,
        RuntimeRequest::GetSessionSnapshot {
            session_id: session_id(),
        },
        RuntimeRequest::RefetchModelList {
            provider_id: String::from("provider"),
        },
        RuntimeRequest::CancelStream {
            session_id: session_id(),
        },
        RuntimeRequest::ListUserInputHistory { limit: 1 },
        RuntimeRequest::ApproveScript {
            session_id: session_id(),
            execution_id: String::from("execution"),
        },
        RuntimeRequest::DenyScript {
            session_id: session_id(),
            execution_id: String::from("execution"),
        },
        RuntimeRequest::FinishStartupSync,
    ] {
        assert!(requests.send(request).is_ok());
    }
    drop(requests);
    route_requests(
        receiver,
        &Lanes {
            queries,
            model_lists,
            commands,
        },
        |_| {},
    );
    assert!(matches!(
        queued.try_iter().collect::<Vec<_>>().as_slice(),
        [
            RuntimeRequest::ListSessions,
            RuntimeRequest::GetSessionSnapshot { .. },
            RuntimeRequest::ListUserInputHistory { .. },
            RuntimeRequest::FinishStartupSync,
        ]
    ));
    assert!(matches!(
        fetches.try_iter().collect::<Vec<_>>().as_slice(),
        [RuntimeRequest::RefetchModelList { .. }]
    ));
    assert!(matches!(
        controls.try_iter().collect::<Vec<_>>().as_slice(),
        [
            RuntimeRequest::CancelStream { .. },
            RuntimeRequest::ApproveScript { .. },
            RuntimeRequest::DenyScript { .. },
        ]
    ));
}
