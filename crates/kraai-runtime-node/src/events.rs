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

struct PendingRead<'a>(&'a EventSubscription);

impl Drop for PendingRead<'_> {
    fn drop(&mut self) {
        if self.0.closed.is_cancelled() {
            self.0.release_receiver();
        }
        self.0.reading.store(false, Ordering::Release);
    }
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
        let _pending_read = PendingRead(self);
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
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests assert after fallible async reads"
)]
mod tests {
    use std::task::{Context, Waker};

    use super::*;

    #[tokio::test]
    async fn reports_lag_and_preserves_sequence() -> napi::Result<()> {
        let (sender, receiver) = broadcast::channel(2);
        let subscription = EventSubscription::new(receiver, CancellationToken::new());
        for sequence in 1..=3 {
            let _ = sender.send(RuntimeEvent {
                sequence,
                event: kraai_runtime::Event::ConfigLoaded,
            });
        }
        assert!(matches!(
            subscription.read().await?,
            EventRead::Lagged { skipped: 1 }
        ));
        assert!(matches!(
            subscription.read().await?,
            EventRead::Event {
                value: RuntimeEvent { sequence: 2, .. }
            }
        ));
        subscription.close();
        assert!(matches!(subscription.read().await?, EventRead::Closed));
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_wakes_pending_read_and_rejects_concurrent_reads() -> napi::Result<()> {
        let (_sender, receiver) = broadcast::channel(2);
        let closed = CancellationToken::new();
        let subscription = EventSubscription::new(receiver, closed.child_token());
        let pending = subscription.read();
        tokio::pin!(pending);
        tokio::select! {
            biased;
            result = &mut pending => { result?; return Err(napi::Error::from_reason("read completed before cancellation")); }
            () = std::future::ready(()) => {}
        }
        assert!(subscription.read().await.is_err());
        closed.cancel();
        assert!(matches!(pending.await?, EventRead::Closed));
        Ok(())
    }

    #[tokio::test]
    async fn close_releases_receiver_without_a_pending_read() -> napi::Result<()> {
        let (sender, receiver) = broadcast::channel(2);
        let subscription = EventSubscription::new(receiver, CancellationToken::new());
        assert_eq!(sender.receiver_count(), 1);
        subscription.close();
        assert_eq!(sender.receiver_count(), 0);
        assert!(matches!(subscription.read().await?, EventRead::Closed));
        Ok(())
    }

    #[tokio::test]
    async fn close_wakes_pending_read_without_allowing_a_second_reader() -> napi::Result<()> {
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
        let error =
            subscription.read().await.err().ok_or_else(|| {
                napi::Error::from_reason("concurrent read was accepted after close")
            })?;
        assert_eq!(
            error.reason,
            "only one pending next() is allowed: operation would block"
        );
        assert!(matches!(pending.await?, EventRead::Closed));
        assert_eq!(sender.receiver_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn abandoned_reads_release_the_permit_and_closed_receiver() -> napi::Result<()> {
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
                assert!(matches!(subscription.read().await?, EventRead::Closed));
            } else {
                sender
                    .send(RuntimeEvent {
                        sequence: 1,
                        event: kraai_runtime::Event::ConfigLoaded,
                    })
                    .map_err(|error| napi::Error::from_reason(error.to_string()))?;
                assert!(matches!(
                    subscription.read().await?,
                    EventRead::Event {
                        value: RuntimeEvent { sequence: 1, .. }
                    }
                ));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn close_mutex_contention_is_not_a_second_pending_read() -> napi::Result<()> {
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

        assert!(matches!(pending.await?, EventRead::Closed));
        assert_eq!(sender.receiver_count(), 0);
        Ok(())
    }
}
