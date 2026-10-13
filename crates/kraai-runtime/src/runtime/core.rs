use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kraai_agent::AgentManager;
use kraai_persistence::{ContextStateStore, ScriptExecutionStore};
use kraai_provider_core::ProviderRegistry;
use kraai_provider_openai_codex::OpenAiCodexAuthController;
use kraai_types::SessionId;
use tokio::sync::{Mutex, mpsc};

use crate::api::Event;
use crate::api::RuntimeStartupState;
use crate::handle::{Command, RuntimeEventSender};

pub(crate) fn emit_event(event_tx: &RuntimeEventSender, event: Event) {
    event_tx.send(event);
}

#[derive(Clone)]
pub(crate) struct RuntimeCore {
    pub(crate) event_tx: RuntimeEventSender,
    pub(crate) command_tx: mpsc::Sender<Command>,
    pub(crate) sessions: Arc<super::session::SessionActors>,
    pub(crate) agent_manager: Arc<AgentManager>,
    pub(crate) observed_sessions:
        Arc<Mutex<HashMap<SessionId, kraai_persistence::SessionObservation>>>,
    pub(crate) session_store: Arc<kraai_persistence::SqliteSessionStore>,
    pub(crate) image_store: Arc<kraai_persistence::FileImageStore>,
    pub(crate) execution_store: Arc<dyn ScriptExecutionStore>,
    pub(crate) context_state_store: Arc<dyn ContextStateStore>,
    pub(crate) provider_registry: ProviderRegistry,
    pub(crate) stream_tasks: Arc<super::stream_tasks::StreamTasks>,
    /// Serializes provider configuration reloads.
    pub(crate) config_reload: Arc<Mutex<()>>,
    pub(crate) stopping: Arc<AtomicBool>,
    pub(crate) openai_codex_auth: Arc<OpenAiCodexAuthController>,
    pub(crate) config: Arc<RuntimeConfig>,
    pub(crate) startup_tx: tokio::sync::watch::Sender<RuntimeStartupState>,
    pub(crate) model_catalog_tx: tokio::sync::watch::Sender<tokio::sync::watch::Receiver<u64>>,
}

#[derive(Clone)]
pub(crate) struct RuntimeConfig {
    pub(crate) provider_config_path: PathBuf,
    pub(crate) nushell_host_path: Option<PathBuf>,
    pub(crate) script_runtime_roots: Option<Vec<PathBuf>>,
    pub(crate) use_current_executable_as_nushell_host: bool,
    /// Holds full script output that the model saw only part of, one directory per session.
    pub(crate) script_output_root: PathBuf,
}

impl RuntimeConfig {
    pub(crate) fn script_output_dir(&self, session_id: &kraai_types::SessionId) -> PathBuf {
        self.script_output_root.join(session_id.as_str())
    }
}

impl RuntimeCore {
    pub(crate) fn send_event(&self, event: Event) {
        emit_event(&self.event_tx, event);
    }

    pub(crate) fn send_service_error(&self, error: impl std::fmt::Display) {
        self.send_event(Event::ServiceError {
            error: crate::RuntimeError::internal(error),
        });
    }

    pub(crate) fn send_session_error(&self, session_id: &SessionId, error: impl std::fmt::Display) {
        self.send_event(Event::SessionError {
            session_id: session_id.clone(),
            error: crate::RuntimeError::internal(error),
        });
    }

    pub(crate) fn send_session_report_error(
        &self,
        session_id: &SessionId,
        error: color_eyre::Report,
    ) {
        self.send_event(Event::SessionError {
            session_id: session_id.clone(),
            error: crate::RuntimeError::from_report(error),
        });
    }

    pub(crate) async fn run(
        self,
        command_rx: mpsc::Receiver<Command>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) {
        tracing::info!("Starting event loop");

        let background_tasks = [
            self.spawn_config_watcher(),
            self.spawn_openai_auth_forwarder(),
            self.spawn_model_catalog_forwarder(),
            self.spawn_mcp_auth_forwarder(),
            self.spawn_lease_heartbeat(),
            self.spawn_session_observers(),
        ];
        match self.load_providers_config_and_emit().await {
            Ok(()) => {
                self.startup_tx.send_replace(RuntimeStartupState::ready());
            }
            Err(error) => {
                let error = format!("Failed to load config: {error}");
                self.startup_tx
                    .send_replace(RuntimeStartupState::Failed(error.clone()));
                self.send_service_error(error);
            }
        }
        self.serve(command_rx, shutdown_rx).await;
        for task in background_tasks {
            task.abort();
            let _ = task.await;
        }
        tracing::info!("Event loop terminated");
    }

    /// Recovers abandoned sessions, then dispatches commands until shutdown and stops all
    /// session work before answering the shutdown request.
    /// Reports every durable history change to clients as it happens.
    fn observe_history(&self) {
        let event_tx = self.event_tx.clone();
        self.agent_manager
            .set_history_observer(Arc::new(move |change| {
                let event = match change {
                    kraai_agent::HistoryChange::Upserted {
                        session_id,
                        message,
                    } => Event::MessageUpserted {
                        session_id,
                        message: Box::new(super::client_view::client_message(*message)),
                    },
                    kraai_agent::HistoryChange::Removed {
                        session_id,
                        message_ids,
                    } => Event::MessagesRemoved {
                        session_id,
                        message_ids,
                    },
                };
                emit_event(&event_tx, event);
            }));
    }

    pub(crate) async fn serve(
        &self,
        mut command_rx: mpsc::Receiver<Command>,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) {
        self.observe_history();
        self.sweep_expired_leases().await;
        let mut shutdown_response = None;
        loop {
            let command = tokio::select! {
                command = command_rx.recv() => command,
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        None
                    } else {
                        continue;
                    }
                }
            };
            let Some(command) = command else {
                break;
            };
            if let Command::Shutdown { response } = command {
                shutdown_response = response;
                break;
            }
            self.dispatch(command);
        }

        self.stop_active_work().await;
        for session in self.session_store.owned_sessions() {
            self.release_turn(&session).await;
        }
        self.shutdown_mcp().await;
        if let Some(response) = shutdown_response {
            let _ = response.send(Ok(()));
        }
    }

    pub(crate) async fn stop_active_work(&self) {
        self.stopping.store(true, Ordering::Release);
        self.sessions.stop().await;
        self.stream_tasks.close();
        self.stream_tasks.wait().await;
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub(crate) async fn load_providers_config_and_emit(&self) -> color_eyre::Result<()> {
        let _reload = self.config_reload.lock().await;
        let config = self
            .read_and_validate_provider_config(&self.config.provider_config_path)
            .await?;
        let agent = &self.agent_manager;
        let result = agent
            .set_providers(config, self.provider_registry.clone())
            .await;
        let updates = agent.cloned_provider_manager().subscribe_model_catalog();
        self.model_catalog_tx.send_if_modified(|current| {
            if current.same_channel(&updates) {
                false
            } else {
                *current = updates;
                true
            }
        });
        result?;
        tracing::info!("Loaded config");
        self.send_event(Event::ConfigLoaded);
        Ok(())
    }
}
