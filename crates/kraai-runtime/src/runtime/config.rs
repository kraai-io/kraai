use std::path::PathBuf;

use color_eyre::eyre::{Result, WrapErr, eyre};
use kraai_provider_core::ProviderManagerConfig;
use kraai_provider_openai_codex::{
    OpenAiCodexAuthStatus as ProviderOpenAiCodexAuthStatus,
    OpenAiCodexLoginState as ProviderOpenAiCodexLoginState,
};
use notify::{RecursiveMode, Watcher};

use super::core::{RuntimeCore, emit_event};
use crate::api::{
    Event, OpenAiCodexAuthStatus, OpenAiCodexLoginState, PendingBrowserLogin,
    PendingDeviceCodeLogin,
};
use crate::handle::Command;
use crate::settings::{SettingsDocument, write_settings_document};

impl RuntimeCore {
    pub(crate) fn spawn_openai_auth_forwarder(&self) {
        let mut updates = self.openai_codex_auth.subscribe();
        let runtime = self.clone();
        tokio::spawn(async move {
            while let Ok(status) = updates.recv().await {
                runtime.send_event(Event::OpenAiCodexAuthUpdated {
                    status: map_openai_codex_auth_status(status),
                });
            }
        });
    }

    pub(crate) fn spawn_config_watcher(&self) {
        let command_tx = self.command_tx.clone();
        let event_tx = self.event_tx.clone();
        let config_loc = self.provider_config_path.clone();

        tokio::spawn(async move {
            let config_dir = match config_loc.parent() {
                Some(path) => path.to_path_buf(),
                None => {
                    emit_event(
                        &event_tx,
                        Event::Error(String::from("Config path has no parent")),
                    );
                    return;
                }
            };
            if let Err(error) = std::fs::create_dir_all(&config_dir) {
                emit_event(
                    &event_tx,
                    Event::Error(format!(
                        "Failed to create config directory {}: {error}",
                        config_dir.display()
                    )),
                );
                return;
            }

            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(watcher) => watcher,
                Err(error) => {
                    emit_event(
                        &event_tx,
                        Event::Error(format!("Failed to create config watcher: {error}")),
                    );
                    return;
                }
            };

            if let Err(error) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
                emit_event(
                    &event_tx,
                    Event::Error(format!(
                        "Failed to watch config directory {}: {error}",
                        config_dir.display()
                    )),
                );
                return;
            }

            for res in rx {
                match res {
                    Ok(event) => {
                        if event.kind.is_access() {
                            continue;
                        }
                        if !event.paths.iter().any(|path| path == &config_loc) {
                            continue;
                        }
                        let _ = command_tx.send(Command::LoadConfig).await;
                    }
                    Err(error) => {
                        emit_event(
                            &event_tx,
                            Event::Error(format!("Config watch error: {error:?}")),
                        );
                    }
                }
            }
        });
    }

    pub(crate) async fn load_providers_config(&self) -> Result<()> {
        let config_loc = &self.provider_config_path;
        let config = if !config_loc.exists() {
            ProviderManagerConfig {
                providers: Vec::new(),
                models: Vec::new(),
            }
        } else {
            let config_slice = tokio::fs::read(config_loc).await?;
            toml::from_slice(&config_slice).wrap_err_with(|| {
                format!("Failed to parse provider config {}", config_loc.display())
            })?
        };

        self.agent_manager
            .lock()
            .await
            .set_providers(config, self.provider_registry.clone())
            .await?;

        Ok(())
    }

    pub(crate) async fn save_settings_document(&self, settings: SettingsDocument) -> Result<()> {
        write_settings_document(
            &self.provider_config_path,
            &settings,
            &self.provider_registry,
        )
        .await?;
        self.load_providers_config().await?;
        tracing::info!("Loaded config");
        self.send_event(Event::ConfigLoaded);
        Ok(())
    }
}

pub(crate) fn canonicalize_workspace_dir(path: &str) -> Result<PathBuf> {
    let raw = PathBuf::from(path);
    if !raw.exists() {
        return Err(eyre!(
            "Workspace directory does not exist: {}",
            raw.display()
        ));
    }
    if !raw.is_dir() {
        return Err(eyre!(
            "Workspace path is not a directory: {}",
            raw.display()
        ));
    }

    Ok(raw.canonicalize().unwrap_or(raw))
}

pub(crate) fn map_openai_codex_auth_status(
    status: ProviderOpenAiCodexAuthStatus,
) -> OpenAiCodexAuthStatus {
    let state = match status.state {
        ProviderOpenAiCodexLoginState::SignedOut => OpenAiCodexLoginState::SignedOut,
        ProviderOpenAiCodexLoginState::BrowserPending(pending) => {
            OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                auth_url: pending.auth_url,
            })
        }
        ProviderOpenAiCodexLoginState::DeviceCodePending(pending) => {
            OpenAiCodexLoginState::DeviceCodePending(PendingDeviceCodeLogin {
                verification_url: pending.verification_url,
                user_code: pending.user_code,
            })
        }
        ProviderOpenAiCodexLoginState::Authenticated => OpenAiCodexLoginState::Authenticated,
    };

    OpenAiCodexAuthStatus {
        state,
        email: status.email,
        plan_type: status.plan_type,
        account_id: status.account_id,
        last_refresh_unix: status.last_refresh_unix,
        error: status.error,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::settings::{default_provider_config_path, resolve_provider_config_path};

    #[test]
    fn resolve_provider_config_path_uses_override_when_present() {
        let override_path = PathBuf::from("/tmp/custom-providers.toml");

        let resolved =
            resolve_provider_config_path(Some(override_path.clone())).expect("path should resolve");

        assert_eq!(resolved, override_path);
    }

    #[test]
    fn resolve_provider_config_path_falls_back_to_default_location() {
        let resolved = resolve_provider_config_path(None).expect("default path should resolve");

        assert_eq!(
            resolved,
            default_provider_config_path().expect("default path should resolve")
        );
    }
}
