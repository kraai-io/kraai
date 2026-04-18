use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use kraai_agent::AgentManager;
use kraai_provider_core::ProviderRegistry;
use kraai_provider_openai_codex::OpenAiCodexAuthController;
use kraai_types::{MessageId, ModelId, ProviderId};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::task::AbortHandle;

use crate::api::Event;
use crate::handle::Command;

pub(crate) fn emit_event(event_tx: &broadcast::Sender<Event>, event: Event) {
    let _ = event_tx.send(event);
}

#[derive(Clone)]
pub(crate) struct RuntimeCore {
    pub(crate) event_tx: broadcast::Sender<Event>,
    pub(crate) command_tx: mpsc::Sender<Command>,
    pub(crate) agent_manager: Arc<Mutex<AgentManager>>,
    pub(crate) provider_registry: ProviderRegistry,
    pub(crate) active_streams: Arc<Mutex<HashMap<String, ActiveStream>>>,
    pub(crate) queued_messages: Arc<Mutex<HashMap<String, VecDeque<QueuedMessage>>>>,
    pub(crate) openai_codex_auth: Arc<OpenAiCodexAuthController>,
    pub(crate) provider_config_path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveStream {
    pub(crate) message_id: MessageId,
    pub(crate) abort_handle: AbortHandle,
}

#[derive(Clone, Debug)]
pub(crate) struct QueuedMessage {
    pub(crate) message: String,
    pub(crate) model_id: ModelId,
    pub(crate) provider_id: ProviderId,
    pub(crate) auto_approve: bool,
}

impl RuntimeCore {
    pub(crate) fn send_event(&self, event: Event) {
        emit_event(&self.event_tx, event);
    }

    pub(crate) fn send_error(&self, error: impl Into<String>) {
        self.send_event(Event::Error(error.into()));
    }

    pub(crate) async fn run(self, mut command_rx: mpsc::Receiver<Command>) {
        tracing::info!("Starting event loop");

        self.spawn_config_watcher();
        self.spawn_openai_auth_forwarder();
        if let Err(error) = self.load_providers_config().await {
            self.send_error(format!("Failed to load config: {error}"));
        } else {
            tracing::info!("Loaded config");
            self.send_event(Event::ConfigLoaded);
        }

        while let Some(command) = command_rx.recv().await {
            if let Err(error) = self.handle_command(command).await {
                self.send_error(error.to_string());
            }
        }

        tracing::info!("Event loop terminated");
    }
}
