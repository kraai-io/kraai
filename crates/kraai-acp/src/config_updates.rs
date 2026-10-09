use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use agent_client_protocol::{Result, schema::v1 as acp};
use kraai_runtime::{Event, RuntimeEvent};
use tokio::sync::{Notify, OwnedMutexGuard, broadcast};

use crate::{Server, config, session::Session, transport::Connection};

pub(crate) type PublishedConfig = Option<Vec<acp::SessionConfigOption>>;

#[derive(Default)]
pub(crate) struct ConfigUpdates {
    wake: Notify,
}

pub(crate) struct Publication {
    pub current: OwnedMutexGuard<PublishedConfig>,
    generation: Arc<AtomicU64>,
    _updated: NotifyOnDrop,
}

impl Drop for Publication {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

struct NotifyOnDrop(Arc<ConfigUpdates>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.wake.notify_one();
    }
}

pub(crate) async fn lock(session: &Session) -> Publication {
    Publication {
        current: session.configuration.clone().lock_owned().await,
        generation: session.config_generation.clone(),
        _updated: NotifyOnDrop(session.config_updated.clone()),
    }
}

pub(crate) struct PreparedSession<T> {
    pub response: T,
    pub session: Arc<Session>,
    pub _publication: Publication,
}

impl<T> PreparedSession<T> {
    pub fn respond(self, send: impl FnOnce(T) -> Result<()>) -> Result<()> {
        send(self.response)?;
        self.session.ready.store(true, Ordering::Release);
        Ok(())
    }
}

pub(crate) async fn run(
    server: &Server,
    connection: &Connection,
    mut events: broadcast::Receiver<RuntimeEvent>,
) -> Result<()> {
    loop {
        let refresh = tokio::select! {
            () = server.config_updated.wake.notified() => true,
            event = events.recv() => match event {
                Ok(event) => matches!(event.event, Event::ModelsUpdated | Event::ConfigLoaded),
                Err(broadcast::error::RecvError::Lagged(_)) => true,
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        };
        if !refresh {
            continue;
        }
        for _ in 0..256 {
            match events.try_recv() {
                Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => return Ok(()),
            }
        }
        let sessions = server
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| session.ready.load(Ordering::Acquire))
            .map(|(id, session)| {
                (
                    id.clone(),
                    session.clone(),
                    session.config_generation.load(Ordering::Acquire),
                )
            })
            .collect::<Vec<_>>();
        if sessions.is_empty() {
            continue;
        }
        let models = match server.runtime.list_models().await {
            Ok(models) => models,
            Err(error) => {
                tracing::warn!(%error, "Could not refresh ACP model configuration");
                continue;
            }
        };
        for (id, session, generation) in sessions {
            let Ok(mut published) = session.configuration.try_lock() else {
                continue;
            };
            if session.config_generation.load(Ordering::Acquire) != generation {
                continue;
            }
            if !session.ready.load(Ordering::Acquire) {
                continue;
            }
            let config = match config::options_with_models(&server.runtime, &id, &models).await {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!(%error, session_id = %id, "Could not refresh ACP session configuration");
                    continue;
                }
            };
            if published.as_ref() != Some(&config) {
                config::notify(connection, acp::SessionId::new(id), config.clone())?;
                *published = Some(config);
            }
        }
    }
}

#[cfg(test)]
mod tests;
