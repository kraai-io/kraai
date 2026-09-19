use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, eyre};
use kraai_provider_core::ProviderManagerConfig;
use kraai_provider_openai_codex::OpenAiCodexAuthStatus;
use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::core::{RuntimeCore, emit_event};
use crate::api::{Event, RuntimeError, RuntimeResult};
use crate::handle::{Command, RuntimeEventSender};
use crate::settings::{
    SettingsDocument, read_provider_config, validate_settings, write_settings_document,
};

impl RuntimeCore {
    pub(crate) fn spawn_openai_auth_forwarder(&self) -> JoinHandle<()> {
        tokio::spawn(forward_auth_updates(
            self.openai_codex_auth.subscribe(),
            self.event_tx.clone(),
        ))
    }

    pub(crate) fn spawn_config_watcher(&self) -> JoinHandle<()> {
        let command_tx = self.command_tx.clone();
        let event_tx = self.event_tx.clone();
        let config_loc = self.provider_config_path.clone();

        tokio::spawn(async move {
            let config_dir = match config_loc.parent() {
                Some(path) => path.to_path_buf(),
                None => {
                    emit_event(
                        &event_tx,
                        Event::ServiceError {
                            error: RuntimeError::internal("Config path has no parent"),
                        },
                    );
                    return;
                }
            };
            if let Err(error) = std::fs::create_dir_all(&config_dir) {
                emit_event(
                    &event_tx,
                    Event::ServiceError {
                        error: RuntimeError::internal(format!(
                            "Failed to create config directory {}: {error}",
                            config_dir.display()
                        )),
                    },
                );
                return;
            }

            let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut watcher = match notify::recommended_watcher(move |result| {
                let _ = notify_tx.send(result);
            }) {
                Ok(watcher) => watcher,
                Err(error) => {
                    emit_event(
                        &event_tx,
                        Event::ServiceError {
                            error: RuntimeError::internal(format!(
                                "Failed to create config watcher: {error}"
                            )),
                        },
                    );
                    return;
                }
            };

            if let Err(error) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
                emit_event(
                    &event_tx,
                    Event::ServiceError {
                        error: RuntimeError::internal(format!(
                            "Failed to watch config directory {}: {error}",
                            config_dir.display()
                        )),
                    },
                );
                return;
            }

            while let Some(res) = notify_rx.recv().await {
                match res {
                    Ok(event) => {
                        if event.kind.is_access() {
                            continue;
                        }
                        if !event.paths.iter().any(|path| path == &config_loc) {
                            continue;
                        }
                        if command_tx.send(Command::LoadConfig).await.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        emit_event(
                            &event_tx,
                            Event::ServiceError {
                                error: RuntimeError::internal(format!(
                                    "Config watch error: {error:?}"
                                )),
                            },
                        );
                    }
                }
            }
        })
    }

    pub(crate) async fn read_and_validate_provider_config(
        &self,
        config_loc: &Path,
    ) -> Result<ProviderManagerConfig> {
        read_provider_config(config_loc, &self.provider_registry).await
    }

    pub(crate) async fn save_settings_document(
        &self,
        settings: SettingsDocument,
    ) -> RuntimeResult<()> {
        let violations = validate_settings(&settings, &self.provider_registry);
        if !violations.is_empty() {
            return Err(RuntimeError::validation(violations));
        }
        write_settings_document(&self.provider_config_path, &settings)
            .await
            .map_err(RuntimeError::internal)?;
        self.load_providers_config_and_emit()
            .await
            .map_err(RuntimeError::internal)?;
        Ok(())
    }
}

pub(crate) fn canonicalize_workspace_dir(path: &str) -> Result<PathBuf> {
    let raw = PathBuf::from(path);
    if !raw.exists() {
        return Err(eyre!(kraai_types::DomainError::invalid_argument(format!(
            "Workspace directory does not exist: {}",
            raw.display()
        ))));
    }
    if !raw.is_dir() {
        return Err(eyre!(kraai_types::DomainError::invalid_argument(format!(
            "Workspace path is not a directory: {}",
            raw.display()
        ))));
    }

    Ok(raw.canonicalize().unwrap_or(raw))
}

async fn forward_auth_updates(
    mut updates: broadcast::Receiver<OpenAiCodexAuthStatus>,
    events: RuntimeEventSender,
) {
    loop {
        match updates.recv().await {
            Ok(status) => events.send(Event::OpenAiCodexAuthUpdated { status }),
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::ensure;
    use kraai_provider_openai_codex::OpenAiCodexLoginState;

    #[tokio::test]
    async fn auth_forwarding_recovers_from_lag_and_drains_before_close() -> Result<()> {
        let (updates, receiver) = broadcast::channel(2);
        let events = RuntimeEventSender::new(4);
        let mut received = events.subscribe();
        let status = |sequence| OpenAiCodexAuthStatus {
            state: OpenAiCodexLoginState::SignedOut,
            email: None,
            plan_type: None,
            account_id: None,
            last_refresh_unix: Some(sequence),
            error: None,
        };
        for sequence in 0..3 {
            updates.send(status(sequence))?;
        }
        let forwarding = forward_auth_updates(receiver, events);
        tokio::pin!(forwarding);
        ensure!(futures::poll!(&mut forwarding).is_pending());
        for sequence in 1..3 {
            ensure!(matches!(received.try_recv()?.event,
                Event::OpenAiCodexAuthUpdated { status: actual } if actual == status(sequence)));
        }
        updates.send(status(3))?;
        drop(updates);
        tokio::time::timeout(std::time::Duration::from_secs(1), forwarding).await?;
        ensure!(matches!(received.try_recv()?.event,
            Event::OpenAiCodexAuthUpdated { status: actual } if actual == status(3)));
        Ok(())
    }
}
