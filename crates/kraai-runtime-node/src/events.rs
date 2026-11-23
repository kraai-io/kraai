use std::mem::DropGuard;
use std::sync::atomic::{AtomicBool, Ordering};

use kraai_runtime::RuntimeEvent;
use napi_derive::napi;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

#[derive(Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "types.d.ts")]
pub(crate) enum EventRead {
    Event { value: RuntimeEvent },
    Lagged { skipped: u64 },
    Closed,
}

#[napi]
pub struct EventSubscription {
    receiver: Mutex<Option<broadcast::Receiver<RuntimeEvent>>>,
    reading: AtomicBool,
    closed: CancellationToken,
}

impl EventSubscription {
    pub(crate) fn new(
        receiver: broadcast::Receiver<RuntimeEvent>,
        closed: CancellationToken,
    ) -> Self {
        Self {
            receiver: Mutex::new(Some(receiver)),
            reading: AtomicBool::new(false),
            closed,
        }
    }

    async fn read(&self) -> napi::Result<EventRead> {
        self.reading
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_reading| {
                napi::Error::from_reason(
                    "only one pending next() is allowed: operation would block",
                )
            })?;
        let _pending_read = DropGuard::new(self, |subscription| {
            if subscription.closed.is_cancelled() {
                subscription.release_receiver();
            }
            subscription.reading.store(false, Ordering::Release);
        });
        let mut receiver = self.receiver.lock().await;
        let Some(events) = receiver.as_mut() else {
            return Ok(EventRead::Closed);
        };
        let event = tokio::select! {
            biased;
            () = self.closed.cancelled() => EventRead::Closed,
            event = events.recv() => match event {
                Ok(value) => EventRead::Event { value },
                Err(broadcast::error::RecvError::Lagged(skipped)) => EventRead::Lagged { skipped },
                Err(broadcast::error::RecvError::Closed) => EventRead::Closed,
            },
        };
        if matches!(event, EventRead::Closed) {
            drop(receiver.take());
        }
        drop(receiver);
        Ok(event)
    }

    fn release_receiver(&self) {
        if let Ok(mut receiver) = self.receiver.try_lock() {
            drop(receiver.take());
        }
    }
}

#[napi]
impl EventSubscription {
    #[napi(skip_typescript)]
    pub async fn next(&self) -> napi::Result<Value> {
        crate::wire::value(self.read().await?)
    }

    #[napi]
    pub fn close(&self) {
        self.closed.cancel();
        self.release_receiver();
    }
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}

#[cfg(test)]
mod tests;
