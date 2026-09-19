use std::future::{Future, poll_fn, ready};
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
async fn buffered_config_events_precede_startup_and_later_reloads_are_forwarded()
-> Result<(), Box<dyn std::error::Error>> {
    let (updates, receiver) = broadcast::channel(8);
    let (sender, messages) = unbounded();
    updates.send(config_event(1))?;
    let mut forwarding = Box::pin(forward_runtime_events(
        receiver,
        sender,
        ready(Ok(RuntimeStartupState::Ready)),
    ));
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv()?,
        RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence: 1, .. })
    ));
    assert!(matches!(
        messages.try_recv()?,
        RuntimeEventBridgeMessage::StartupComplete(Ok(RuntimeStartupState::Ready))
    ));
    updates.send(config_event(2))?;
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv()?,
        RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence: 2, .. })
    ));
    assert!(messages.is_empty());
    drop(updates);
    forwarding.await;
    Ok(())
}

#[tokio::test]
async fn retained_startup_result_covers_late_subscribers_and_failure()
-> Result<(), Box<dyn std::error::Error>> {
    for state in [
        RuntimeStartupState::Ready,
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
            messages.try_recv()?,
            RuntimeEventBridgeMessage::StartupComplete(Ok(actual)) if actual == state
        ));
        assert!(messages.is_empty());
        drop(updates);
        forwarding.await;
    }
    Ok(())
}

#[tokio::test]
async fn startup_lag_is_reported_before_the_completion_marker()
-> Result<(), Box<dyn std::error::Error>> {
    let (updates, receiver) = broadcast::channel(2);
    let (sender, messages) = unbounded();
    for sequence in 1..=3 {
        updates.send(config_event(sequence))?;
    }
    let mut forwarding = Box::pin(forward_runtime_events(
        receiver,
        sender,
        ready(Ok(RuntimeStartupState::Ready)),
    ));
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv()?,
        RuntimeEventBridgeMessage::Lagged(1)
    ));
    for sequence in 2..=3 {
        assert!(matches!(
            messages.try_recv()?,
            RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence: actual, .. }) if actual == sequence
        ));
    }
    assert!(matches!(
        messages.try_recv()?,
        RuntimeEventBridgeMessage::StartupComplete(Ok(RuntimeStartupState::Ready))
    ));
    drop(updates);
    forwarding.await;
    Ok(())
}

#[tokio::test]
async fn closed_events_cannot_leave_startup_waiting_forever()
-> Result<(), Box<dyn std::error::Error>> {
    let (updates, receiver) = broadcast::channel(2);
    drop(updates);
    let (sender, messages) = unbounded();
    forward_runtime_events(receiver, sender, std::future::pending()).await;
    assert!(matches!(
        messages.try_recv()?,
        RuntimeEventBridgeMessage::StartupComplete(Err(_))
    ));
    Ok(())
}

#[tokio::test]
async fn runtime_events_remain_live_while_startup_is_pending()
-> Result<(), Box<dyn std::error::Error>> {
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
    updates.send(config_event(1))?;
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv()?,
        RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence: 1, .. })
    ));
    assert!(messages.is_empty());
    assert!(started.send(RuntimeStartupState::Ready).is_ok());
    poll_pending(forwarding.as_mut()).await;
    assert!(matches!(
        messages.try_recv()?,
        RuntimeEventBridgeMessage::StartupComplete(Ok(RuntimeStartupState::Ready))
    ));
    drop(updates);
    forwarding.await;
    Ok(())
}

#[test]
fn startup_drain_finishes_even_when_every_forwarded_event_is_replaced()
-> Result<(), Box<dyn std::error::Error>> {
    let (updates, mut receiver) = broadcast::channel(2);
    updates.send(config_event(1))?;
    let mut forwarded = 0;
    let drained = drain_startup_events(&mut receiver, |event| {
        forwarded += 1;
        assert!(matches!(event, Ok(RuntimeEvent { sequence, .. }) if sequence == forwarded));
        assert!(updates.send(config_event(forwarded + 1)).is_ok());
        forwarded < 16
    });
    assert!(drained);
    assert_eq!(forwarded, 1);
    assert_eq!(receiver.try_recv()?.sequence, 2);
    Ok(())
}

#[test]
fn startup_drain_counts_overwritten_messages_against_the_original_backlog()
-> Result<(), Box<dyn std::error::Error>> {
    let (updates, mut receiver) = broadcast::channel(2);
    for sequence in 1..=3 {
        updates.send(config_event(sequence))?;
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
    assert_eq!(receiver.try_recv()?.sequence, 4);
    Ok(())
}

#[tokio::test]
async fn request_bridge_finishes_startup_after_empty_and_failed_responses()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let config = root.path().join("providers.toml");
    std::fs::write(&config, "")?;
    let runtime = kraai_runtime::RuntimeBuilder::new()
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
            session_id: String::from("missing"),
        },
        RuntimeRequest::FinishStartupSync,
    ] {
        assert!(requests.send(request).is_ok());
    }
    let timeout = std::time::Duration::from_secs(5);
    assert!(
        matches!(responses.recv_timeout(timeout)?, RuntimeResponse::Models(Ok(models)) if models.is_empty())
    );
    assert!(
        matches!(responses.recv_timeout(timeout)?, RuntimeResponse::Sessions(Ok(sessions)) if sessions.is_empty())
    );
    assert!(
        matches!(responses.recv_timeout(timeout)?, RuntimeResponse::UserInputHistory(Ok(history)) if history.is_empty())
    );
    assert!(matches!(
        responses.recv_timeout(timeout)?,
        RuntimeResponse::AgentProfileCatalog(_)
    ));
    assert!(
        matches!(responses.recv_timeout(timeout)?, RuntimeResponse::SessionSnapshot { result, .. } if result.is_err())
    );
    assert!(matches!(
        responses.recv_timeout(timeout)?,
        RuntimeResponse::StartupSyncComplete
    ));
    assert!(responses.is_empty());
    drop(requests);
    tokio::time::timeout(timeout, runtime.shutdown()).await??;
    Ok(())
}
