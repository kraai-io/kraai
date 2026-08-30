use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use color_eyre::eyre::{Result, eyre};
use futures::stream::{self, BoxStream};
use kraai_agent::AgentManager;
use kraai_persistence::{FileMessageStore, FileScriptExecutionStore, FileSessionStore};
use kraai_provider_core::{ModelConfig, ProviderManager, ProviderRequest};
use kraai_types::{AssistantPhase, ModelId, ProviderId, TokenUsage};
use tokio::sync::{Mutex, broadcast, mpsc};

use super::super::builder::build_provider_registry;
use super::super::core::RuntimeCore;
use crate::handle::Command;
use crate::{Event, EventCallback, RuntimeHandle};

fn is_missing_system_ca_error(error: &dyn std::error::Error) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        let display = error.to_string();
        let debug = format!("{error:?}");
        if display.contains("No CA certificates were loaded from the system")
            || debug.contains("No CA certificates were loaded from the system")
            || display == "builder error"
        {
            return true;
        }
        current = error.source();
    }
    false
}

#[derive(Clone, Debug)]
enum ScriptedChunkKind {
    Text { phase: AssistantPhase, text: String },
    NativeCall { call_id: String, input: String },
    Usage(TokenUsage),
    Error(String),
}

#[derive(Clone, Debug)]
pub(super) struct ScriptedChunk {
    kind: ScriptedChunkKind,
}

impl ScriptedChunk {
    pub(super) fn plain(text: impl Into<String>) -> Self {
        Self {
            kind: ScriptedChunkKind::Text {
                phase: AssistantPhase::FinalAnswer,
                text: text.into(),
            },
        }
    }

    pub(super) fn commentary(text: impl Into<String>) -> Self {
        Self {
            kind: ScriptedChunkKind::Text {
                phase: AssistantPhase::Commentary,
                text: text.into(),
            },
        }
    }

    pub(super) fn usage(usage: TokenUsage) -> Self {
        Self {
            kind: ScriptedChunkKind::Usage(usage),
        }
    }

    pub(super) fn native_call(call_id: &str, input: impl Into<String>) -> Self {
        Self {
            kind: ScriptedChunkKind::NativeCall {
                call_id: call_id.to_string(),
                input: input.into(),
            },
        }
    }

    pub(super) fn error(error: impl Into<String>) -> Self {
        Self {
            kind: ScriptedChunkKind::Error(error.into()),
        }
    }
}

struct ScriptedProvider {
    id: ProviderId,
    scripts: StdMutex<VecDeque<Vec<ScriptedChunk>>>,
    native_custom_tool: bool,
}

#[async_trait]
impl kraai_provider_core::Provider for ScriptedProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![mock_model()]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    fn script_tool_transport(
        &self,
        _model_id: &ModelId,
    ) -> kraai_provider_core::ScriptToolTransport {
        if self.native_custom_tool {
            kraai_provider_core::ScriptToolTransport::NativeCustom
        } else {
            kraai_provider_core::ScriptToolTransport::TextEnvelope
        }
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        request: ProviderRequest,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        if self.native_custom_tool {
            let tool = request
                .script_tool
                .as_ref()
                .ok_or_else(|| eyre!("native scripted provider did not receive a tool"))?;
            if tool.name != "kraai_nushell" {
                return Err(eyre!("unexpected native tool name: {}", tool.name));
            }
        } else if request.script_tool.is_some() {
            return Err(eyre!("text scripted provider received a native tool"));
        }

        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .ok_or_else(|| eyre!("no scripted stream remaining"))?;

        Ok(Box::pin(stream::iter(script.into_iter().map(
            |chunk| match chunk.kind {
                ScriptedChunkKind::Text { phase, text } => {
                    Ok(kraai_provider_core::ProviderStreamEvent::TextDelta {
                        item_id: String::from("scripted-message"),
                        phase,
                        delta: text,
                    })
                }
                ScriptedChunkKind::NativeCall { call_id, input } => {
                    Ok(kraai_provider_core::ProviderStreamEvent::ScriptCall {
                        call_id: kraai_types::ToolCallId::new(call_id),
                        name: String::from("kraai_nushell"),
                        input,
                    })
                }
                ScriptedChunkKind::Usage(usage) => {
                    Ok(kraai_provider_core::ProviderStreamEvent::Usage(usage))
                }
                ScriptedChunkKind::Error(error) => Err(eyre!(error)),
            },
        ))))
    }
}

pub(super) struct RetryNotifyingProvider {
    pub(super) id: ProviderId,
}

#[async_trait]
impl kraai_provider_core::Provider for RetryNotifyingProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![mock_model()]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        _request: ProviderRequest,
        request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        notify_retry(request_context);
        Ok(Box::pin(stream::once(async {
            Ok(kraai_provider_core::ProviderStreamEvent::TextDelta {
                item_id: String::from("retry-message"),
                phase: AssistantPhase::FinalAnswer,
                delta: String::from("provider started"),
            })
        })))
    }
}

fn mock_model() -> kraai_provider_core::Model {
    kraai_provider_core::Model {
        id: ModelId::new("mock-model"),
        name: String::from("Mock Model"),
        max_context: None,
    }
}

fn notify_retry(request_context: &kraai_provider_core::ProviderRequestContext) {
    if let Some(observer) = request_context.retry_observer() {
        observer.on_retry_scheduled(&kraai_provider_core::ProviderRetryEvent {
            operation: "responses",
            retry_number: 1,
            delay: Duration::from_secs(1),
            reason: String::from("HTTP 429"),
        });
    }
}

#[derive(Clone, Default)]
pub(super) struct EventCollector {
    events: Arc<StdMutex<Vec<Event>>>,
}

impl EventCollector {
    pub(super) fn snapshot(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(super) async fn wait_for<F>(&self, description: &str, predicate: F) -> Vec<Event>
    where
        F: Fn(&[Event]) -> bool,
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let snapshot = self.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {description}. Events so far: {snapshot:#?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl EventCallback for EventCollector {
    fn on_event(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }
}

pub(super) struct RuntimeTestHarness {
    pub(super) handle: RuntimeHandle,
    pub(super) events: EventCollector,
    pub(super) runtime: RuntimeCore,
    runtime_task: tokio::task::JoinHandle<()>,
    event_task: tokio::task::JoinHandle<()>,
    pub(super) data_dir: PathBuf,
}

impl RuntimeTestHarness {
    pub(super) async fn new(scripts: Vec<Vec<ScriptedChunk>>) -> Option<Self> {
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock"),
            Box::new(ScriptedProvider {
                id: ProviderId::new("mock"),
                scripts: StdMutex::new(scripts.into()),
                native_custom_tool: false,
            }),
        );
        Self::new_with_parts(providers).await
    }

    pub(super) async fn new_native(scripts: Vec<Vec<ScriptedChunk>>) -> Option<Self> {
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock-native"),
            Box::new(ScriptedProvider {
                id: ProviderId::new("mock-native"),
                scripts: StdMutex::new(scripts.into()),
                native_custom_tool: true,
            }),
        );
        Self::new_with_parts(providers).await
    }

    pub(super) async fn new_with_parts(providers: ProviderManager) -> Option<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let data_dir =
            std::env::temp_dir().join(format!("kraai-runtime-test-{}-{nanos}", std::process::id()));
        tokio::fs::create_dir_all(data_dir.join("workspace/.kraai"))
            .await
            .expect("create test workspace");
        tokio::fs::write(
            data_dir.join("workspace/.kraai/agents.toml"),
            "[[profiles]]\n\
id = \"test-profile\"\n\
display_name = \"Test Profile\"\n\
description = \"Runtime test profile\"\n\
system_prompt = \"Runtime test profile\"\n\
commands = []\n\
capabilities = [\"workspace-read\"]\n\
escalation_policy = \"prompt\"\n\
environment = \"allow-list\"\n\
nushell_startup = \"clean\"\n\
path = \"inherit\"\n",
        )
        .await
        .expect("write test profile");

        let message_store = Arc::new(FileMessageStore::new(&data_dir));
        let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
        let execution_store = Arc::new(FileScriptExecutionStore::new(&data_dir));
        let context_state_store =
            Arc::new(kraai_persistence::FileContextStateStore::new(&data_dir));
        let agent_manager = Arc::new(Mutex::new(AgentManager::new(
            providers,
            data_dir.join("workspace"),
            message_store,
            session_store,
            context_state_store.clone(),
        )));

        let openai_codex_auth = match kraai_provider_openai_codex::OpenAiCodexAuthController::new()
        {
            Ok(controller) => Arc::new(controller),
            Err(error) if is_missing_system_ca_error(&error) => return None,
            Err(error) => panic!("unexpected OpenAI auth controller init error: {error}"),
        };
        let events = EventCollector::default();
        let (command_tx, mut command_rx) = mpsc::channel(32);
        let (event_tx, _) = broadcast::channel(1024);
        let (startup_tx, startup_rx) =
            tokio::sync::watch::channel(crate::RuntimeStartupState::Ready);
        let handle = RuntimeHandle {
            command_tx: command_tx.clone(),
            event_tx: event_tx.clone(),
            lifecycle: None,
            startup_rx,
        };
        let runtime = RuntimeCore {
            event_tx: event_tx.clone(),
            command_tx,
            agent_manager,
            execution_store,
            context_state_store,
            provider_registry: build_provider_registry(openai_codex_auth.clone())
                .expect("provider registry"),
            active_streams: Arc::new(Mutex::new(HashMap::new())),
            active_script_tasks: Arc::new(Mutex::new(HashMap::new())),
            pending_script_approvals: Arc::new(Mutex::new(HashMap::new())),
            queued_messages: Arc::new(Mutex::new(HashMap::new())),
            openai_codex_auth,
            provider_config_path: data_dir.join("providers.toml"),
            startup_tx,
        };

        let events_for_task = events.clone();
        let mut event_rx = event_tx.subscribe();
        let event_task = tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(event) => events_for_task.on_event(event),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let runtime_for_task = runtime.clone();
        let runtime_task = tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                if let Command::Shutdown { response } = command {
                    runtime_for_task.stop_active_work().await;
                    if let Some(response) = response {
                        let _ = response.send(());
                    }
                    break;
                }
                if let Err(error) = runtime_for_task.handle_command(command).await {
                    runtime_for_task.send_error(error.to_string());
                }
            }
        });

        Some(Self {
            handle,
            events,
            runtime,
            runtime_task,
            event_task,
            data_dir,
        })
    }

    pub(super) async fn shutdown(self) {
        let _ = self.handle.shutdown().await;
        self.event_task.abort();
        let _ = self.event_task.await;
        let _ = self.runtime_task.await;
        let _ = tokio::fs::remove_dir_all(self.data_dir).await;
    }
}

pub(super) async fn create_session_with_profile(
    handle: &RuntimeHandle,
    profile_id: &str,
) -> Result<String> {
    let session_id = handle.create_session().await?;
    handle
        .set_session_profile(session_id.clone(), profile_id.to_string())
        .await?;
    Ok(session_id)
}
