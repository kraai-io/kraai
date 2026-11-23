use std::task::{Context, Waker};

use super::*;

#[tokio::test]
async fn reports_lag_and_preserves_sequence() {
    let (sender, receiver) = broadcast::channel(2);
    let subscription = EventSubscription::new(receiver, CancellationToken::new());
    for sequence in 1..=3 {
        let _ = sender.send(RuntimeEvent {
            sequence,
            event: kraai_runtime::Event::ConfigLoaded,
        });
    }
    assert!(matches!(
        subscription.read().await.unwrap(),
        EventRead::Lagged { skipped: 1 }
    ));
    assert!(matches!(
        subscription.read().await.unwrap(),
        EventRead::Event {
            value: RuntimeEvent { sequence: 2, .. }
        }
    ));
    subscription.close();
    assert!(matches!(
        subscription.read().await.unwrap(),
        EventRead::Closed
    ));
}

#[tokio::test]
async fn cancellation_wakes_pending_read_and_rejects_concurrent_reads() {
    let (_sender, receiver) = broadcast::channel(2);
    let closed = CancellationToken::new();
    let subscription = EventSubscription::new(receiver, closed.child_token());
    let pending = subscription.read();
    tokio::pin!(pending);
    tokio::select! {
        biased;
        result = &mut pending => { result.unwrap(); panic!("read completed before cancellation"); }
        () = std::future::ready(()) => {}
    }
    assert!(subscription.read().await.is_err());
    closed.cancel();
    assert!(matches!(pending.await.unwrap(), EventRead::Closed));
}

#[tokio::test]
async fn close_releases_receiver_without_a_pending_read() {
    let (sender, receiver) = broadcast::channel(2);
    let subscription = EventSubscription::new(receiver, CancellationToken::new());
    assert_eq!(sender.receiver_count(), 1);
    subscription.close();
    assert_eq!(sender.receiver_count(), 0);
    assert!(matches!(
        subscription.read().await.unwrap(),
        EventRead::Closed
    ));
}

#[tokio::test]
async fn close_wakes_pending_read_without_allowing_a_second_reader() {
    let (sender, receiver) = broadcast::channel(2);
    let subscription = EventSubscription::new(receiver, CancellationToken::new());
    let mut pending = Box::pin(subscription.read());
    assert!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );

    subscription.close();
    let error = subscription
        .read()
        .await
        .err()
        .expect("concurrent read was accepted after close");
    assert_eq!(
        error.reason,
        "only one pending next() is allowed: operation would block"
    );
    assert!(matches!(pending.await.unwrap(), EventRead::Closed));
    assert_eq!(sender.receiver_count(), 0);
}

#[tokio::test]
async fn abandoned_reads_release_the_permit_and_closed_receiver() {
    for close in [false, true] {
        let (sender, receiver) = broadcast::channel(2);
        let subscription = EventSubscription::new(receiver, CancellationToken::new());
        let mut pending = Box::pin(subscription.read());
        assert!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        if close {
            subscription.close();
        }
        drop(pending);

        assert_eq!(sender.receiver_count(), usize::from(!close));
        if close {
            assert!(matches!(
                subscription.read().await.unwrap(),
                EventRead::Closed
            ));
        } else {
            sender
                .send(RuntimeEvent {
                    sequence: 1,
                    event: kraai_runtime::Event::ConfigLoaded,
                })
                .unwrap();
            assert!(matches!(
                subscription.read().await.unwrap(),
                EventRead::Event {
                    value: RuntimeEvent { sequence: 1, .. }
                }
            ));
        }
    }
}

#[tokio::test]
async fn close_mutex_contention_is_not_a_second_pending_read() {
    let (sender, receiver) = broadcast::channel(2);
    let subscription = EventSubscription::new(receiver, CancellationToken::new());
    let receiver_guard = subscription.receiver.lock().await;
    subscription.close();
    let mut pending = Box::pin(subscription.read());
    assert!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(receiver_guard);

    assert!(matches!(pending.await.unwrap(), EventRead::Closed));
    assert_eq!(sender.receiver_count(), 0);
}
