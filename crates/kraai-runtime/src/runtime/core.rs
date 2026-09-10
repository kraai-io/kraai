use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use kraai_agent::AgentManager;
use kraai_persistence::{ContextStateStore, ScriptExecutionStore};
use kraai_provider_core::ProviderRegistry;
use kraai_provider_openai_codex::OpenAiCodexAuthController;
use kraai_types::{MessageId, ModelId, ProviderId};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::sync::CancellationToken;

use super::script_execution::PendingScriptApproval;
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
    pub(crate) queue_drains: Arc<super::queue::QueueDrains>,
    pub(crate) session_preparations: Arc<super::queue::SessionPreparations>,
    pub(crate) agent_manager: Arc<RwLock<AgentManager>>,
    pub(crate) execution_store: Arc<dyn ScriptExecutionStore>,
    pub(crate) context_state_store: Arc<dyn ContextStateStore>,
    pub(crate) provider_registry: ProviderRegistry,
    pub(crate) active_streams: Arc<Mutex<HashMap<String, ActiveStream>>>,
    pub(crate) active_script_tasks: Arc<Mutex<HashMap<String, ActiveScriptTask>>>,
    pub(crate) pending_script_approvals: Arc<Mutex<HashMap<String, PendingScriptApproval>>>,
    pub(crate) queued_messages: Arc<Mutex<HashMap<String, VecDeque<QueuedMessage>>>>,
    /// Separates coherent snapshot reads from state mutations and their corresponding events.
    pub(crate) session_state_barrier: Arc<RwLock<()>>,
    pub(crate) openai_codex_auth: Arc<OpenAiCodexAuthController>,
    pub(crate) provider_config_path: PathBuf,
    pub(crate) use_current_executable_as_nushell_host: bool,
    pub(crate) startup_tx: tokio::sync::watch::Sender<RuntimeStartupState>,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveStream {
    pub(crate) message_id: MessageId,
    pub(crate) abort_handle: AbortHandle,
}

pub(crate) struct ActiveScriptTask {
    pub(crate) cancellation: CancellationToken,
    pub(crate) completion: CancellationToken,
    pub(crate) join_handle: JoinHandle<()>,
}

#[derive(Clone, Debug)]
pub(crate) struct QueuedMessage {
    pub(crate) message: String,
    pub(crate) model_id: ModelId,
    pub(crate) provider_id: ProviderId,
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

    pub(crate) fn send_session_error(
        &self,
        session_id: impl Into<String>,
        error: impl std::fmt::Display,
    ) {
        self.send_event(Event::SessionError {
            session_id: session_id.into(),
            error: crate::RuntimeError::internal(error),
        });
    }

    pub(crate) fn send_session_report_error(
        &self,
        session_id: impl Into<String>,
        error: color_eyre::Report,
    ) {
        self.send_event(Event::SessionError {
            session_id: session_id.into(),
            error: crate::RuntimeError::from_report(error),
        });
    }

    pub(crate) async fn run(
        self,
        mut command_rx: mpsc::Receiver<Command>,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) {
        tracing::info!("Starting event loop");

        let background_tasks = [
            self.spawn_config_watcher(),
            self.spawn_openai_auth_forwarder(),
        ];
        match self.load_providers_config_and_emit().await {
            Ok(()) => match self.recover_script_executions().await {
                Ok(()) => {
                    self.startup_tx.send_replace(RuntimeStartupState::Ready);
                }
                Err(error) => {
                    let error = format!("Failed to recover script executions: {error:#}");
                    self.startup_tx
                        .send_replace(RuntimeStartupState::Failed(error.clone()));
                    self.send_service_error(error);
                }
            },
            Err(error) => {
                let error = format!("Failed to load config: {error}");
                self.startup_tx
                    .send_replace(RuntimeStartupState::Failed(error.clone()));
                self.send_service_error(error);
            }
        }

        let mut shutdown_response = None;
        loop {
            let command = tokio::select! {
                command = self.next_command(&mut command_rx) => command,
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
            if let Err(error) = self.handle_command(command).await {
                self.send_service_error(error);
            }
        }

        self.stop_active_work().await;

        for task in background_tasks {
            task.abort();
            let _ = task.await;
        }
        if let Some(response) = shutdown_response {
            let _ = response.send(Ok(()));
        }

        tracing::info!("Event loop terminated");
    }

    pub(crate) async fn stop_active_work(&self) {
        let active_streams = self
            .active_streams
            .lock()
            .await
            .drain()
            .map(|(_, stream)| stream)
            .collect::<Vec<_>>();
        for stream in active_streams {
            stream.abort_handle.abort();
        }
        let active_script_tasks = self
            .active_script_tasks
            .lock()
            .await
            .drain()
            .map(|(_, task)| task)
            .collect::<Vec<_>>();
        for task in &active_script_tasks {
            task.cancellation.cancel();
        }
        for task in active_script_tasks {
            let _ = task.join_handle.await;
        }
    }

    pub(crate) async fn load_providers_config_and_emit(&self) -> color_eyre::Result<()> {
        let config = self
            .read_and_validate_provider_config(&self.provider_config_path)
            .await?;
        self.agent_manager
            .write()
            .await
            .set_providers(config, self.provider_registry.clone())
            .await?;
        tracing::info!("Loaded config");
        self.send_event(Event::ConfigLoaded);
        Ok(())
    }
}
