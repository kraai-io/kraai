use std::pin::pin;
use std::task::{Context, Waker};

use super::*;

#[tokio::test]
async fn waiters_keep_the_same_lock_across_registry_cleanup() {
    let locks = KeyedLocks::<String>::default();
    let held = locks.lock("shared").await;
    let mut waiting = pin!(locks.lock("shared"));
    assert!(
        waiting
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(held);

    for index in 0..CLEANUP_INTERVAL * 2 {
        drop(locks.lock(&format!("other-{index}")).await);
    }

    let mut competing = pin!(locks.lock("shared"));
    assert!(
        competing
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    let held = waiting.await;
    assert!(
        competing
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    drop(held);
    drop(competing.await);
    assert!(locks.registry.read().await.locks.len() <= CLEANUP_INTERVAL + 1);
}

#[tokio::test]
async fn cleanup_reclaims_capacity_after_a_concurrent_peak() {
    let locks = KeyedLocks::<String>::default();
    let peak = CLEANUP_INTERVAL * 16;
    let mut held = Vec::new();
    for index in 0..peak {
        held.push(locks.lock(&format!("peak-{index}")).await);
    }
    assert!(locks.registry.read().await.locks.capacity() >= peak);
    drop(held);

    for index in 0..peak {
        drop(locks.lock(&format!("after-{index}")).await);
    }

    let registry = locks.registry.read().await;
    let retained_count = registry.locks.len();
    let retained_capacity = registry.locks.capacity();
    drop(registry);
    assert!(retained_count <= CLEANUP_INTERVAL);
    assert!(retained_capacity <= CLEANUP_INTERVAL * 4);
}
