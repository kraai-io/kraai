use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::core::RuntimeCore;
use crate::Event;
use crate::handle::RuntimeEventSender;

impl RuntimeCore {
    pub(crate) fn spawn_model_catalog_forwarder(&self) -> JoinHandle<()> {
        tokio::spawn(forward_model_catalog_updates(
            self.model_catalog_tx.subscribe(),
            self.event_tx.clone(),
        ))
    }
}

async fn forward_model_catalog_updates(
    mut catalogs: watch::Receiver<watch::Receiver<u64>>,
    events: RuntimeEventSender,
) {
    let mut updates = catalogs.borrow().clone();
    let mut source_open = true;
    loop {
        tokio::select! {
            biased;
            changed = catalogs.changed() => {
                if changed.is_err() {
                    return;
                }
                updates = catalogs.borrow_and_update().clone();
                updates.borrow_and_update();
                source_open = true;
                events.send(Event::ModelsUpdated);
            }
            changed = updates.changed(), if source_open => {
                if changed.is_err() {
                    source_open = false;
                } else {
                    updates.borrow_and_update();
                    events.send(Event::ModelsUpdated);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
