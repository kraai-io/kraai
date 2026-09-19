use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Weak};

use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

const CLEANUP_INTERVAL: usize = 64;

pub(crate) struct KeyedLocks<K> {
    registry: RwLock<Registry<K>>,
}

struct Registry<K> {
    locks: HashMap<K, Weak<Mutex<()>>>,
    insertions_until_cleanup: usize,
}

impl<K> Default for KeyedLocks<K> {
    fn default() -> Self {
        Self {
            registry: RwLock::new(Registry {
                locks: HashMap::new(),
                insertions_until_cleanup: CLEANUP_INTERVAL,
            }),
        }
    }
}

impl<K: Eq + Hash + Send + Sync> KeyedLocks<K> {
    pub(crate) async fn lock<Q>(&self, key: &Q) -> OwnedMutexGuard<()>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ToOwned<Owned = K> + Sync + ?Sized,
    {
        let existing = self
            .registry
            .read()
            .await
            .locks
            .get(key)
            .and_then(Weak::upgrade);
        let lock = if let Some(lock) = existing {
            lock
        } else {
            let mut registry = self.registry.write().await;
            let lock = if let Some(lock) = registry.locks.get(key).and_then(Weak::upgrade) {
                lock
            } else {
                if registry.insertions_until_cleanup == 0 {
                    registry.locks.retain(|_, lock| lock.strong_count() > 0);
                    let retained_capacity = registry.locks.len().max(CLEANUP_INTERVAL);
                    if registry.locks.capacity() > retained_capacity.saturating_mul(4) {
                        registry.locks.shrink_to(retained_capacity);
                    }
                    registry.insertions_until_cleanup = retained_capacity;
                }
                let lock = Arc::new(Mutex::new(()));
                registry.locks.insert(key.to_owned(), Arc::downgrade(&lock));
                registry.insertions_until_cleanup -= 1;
                lock
            };
            drop(registry);
            lock
        };
        lock.lock_owned().await
    }
}

#[cfg(test)]
mod tests {
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
}
