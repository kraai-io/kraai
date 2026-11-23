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
mod tests;
