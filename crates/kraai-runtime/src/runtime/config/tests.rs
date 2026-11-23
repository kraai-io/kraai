use super::*;
use kraai_provider_core::ProviderLoginState;
use kraai_test_support::hang_guard;

#[tokio::test]
async fn auth_forwarding_recovers_from_lag_and_drains_before_close() {
    let (updates, receiver) = broadcast::channel(2);
    let events = RuntimeEventSender::new(4);
    let mut received = events.subscribe();
    let status = |sequence| ProviderAuthStatus {
        provider_type: String::from("example"),
        sequence,
        state: ProviderLoginState::SignedOut,
        account: None,
        plan: None,
        last_refresh_unix: Some(sequence),
        error: None,
    };
    for sequence in 0..3 {
        updates.send(status(sequence)).unwrap();
    }
    let forwarding = forward_auth_updates(receiver, events);
    tokio::pin!(forwarding);
    assert!(futures::poll!(&mut forwarding).is_pending());
    for sequence in 1..3 {
        assert!(matches!(received.try_recv().unwrap().event,
            Event::ProviderAuthUpdated { status: actual } if actual == status(sequence)));
    }
    updates.send(status(3)).unwrap();
    drop(updates);
    hang_guard(forwarding).await.unwrap();
    assert!(matches!(received.try_recv().unwrap().event,
        Event::ProviderAuthUpdated { status: actual } if actual == status(3)));
}
