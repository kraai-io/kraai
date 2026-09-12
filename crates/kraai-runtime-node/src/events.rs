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
    receiver: Mutex<broadcast::Receiver<RuntimeEvent>>,
    closed: CancellationToken,
}

impl EventSubscription {
    pub(crate) fn new(
        receiver: broadcast::Receiver<RuntimeEvent>,
        closed: CancellationToken,
    ) -> Self {
        Self {
            receiver: Mutex::new(receiver),
            closed,
        }
    }

    async fn read(&self) -> napi::Result<EventRead> {
        let mut receiver = self.receiver.try_lock().map_err(|error| {
            napi::Error::from_reason(format!("only one pending next() is allowed: {error}"))
        })?;
        let event = tokio::select! {
            biased;
            () = self.closed.cancelled() => EventRead::Closed,
            event = receiver.recv() => match event {
                Ok(value) => EventRead::Event { value },
                Err(broadcast::error::RecvError::Lagged(skipped)) => EventRead::Lagged { skipped },
                Err(broadcast::error::RecvError::Closed) => EventRead::Closed,
            },
        };
        drop(receiver);
        Ok(event)
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
}
