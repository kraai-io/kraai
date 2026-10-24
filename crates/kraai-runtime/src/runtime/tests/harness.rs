use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use color_eyre::eyre::{Result, eyre};
use futures::stream::{self, BoxStream};
use kraai_agent::AgentManager;
use kraai_persistence::SqliteMessageStore;
use kraai_provider_core::{ModelConfig, ProviderManager, ProviderRequest};
use kraai_types::{AssistantPhase, ModelId, ProviderId, SessionId, TokenUsage};
use tokio::sync::{broadcast, mpsc};

use super::super::core::{RuntimeConfig, RuntimeCore};
use crate::handle::RuntimeEventSender;
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
    Reasoning(serde_json::Value),
    Text { phase: AssistantPhase, text: String },
    NativeCall { call_id: String, input: String },
    Usage(TokenUsage),
    Error(String),
    ErrorWithSource(String, String),
    Interrupted(String),
}

#[derive(Clone, Debug)]
pub(super) struct ScriptedChunk {
    kind: ScriptedChunkKind,
}

impl ScriptedChunk {
    pub(super) fn reasoning(payload: serde_json::Value) -> Self {
        Self {
            kind: ScriptedChunkKind::Reasoning(payload),
        }
    }

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

    pub(super) fn error_with_source(error: &str, source: &str) -> Self {
        Self {
            kind: ScriptedChunkKind::ErrorWithSource(error.into(), source.into()),
        }
    }

    pub(super) fn interrupted(reason: impl Into<String>) -> Self {
        Self {
            kind: ScriptedChunkKind::Interrupted(reason.into()),
        }
    }
}

struct ScriptedProvider {
    id: ProviderId,
    scripts: StdMutex<VecDeque<Vec<ScriptedChunk>>>,
}

#[async_trait]
impl kraai_provider_core::Provider for ScriptedProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![mock_model()]
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        request: ProviderRequest,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        let tool = request
            .script_tool
            .as_ref()
            .ok_or_else(|| eyre!("scripted provider did not receive a tool"))?;
        if tool.name != "kraai_nushell" {
            return Err(eyre!("unexpected tool name: {}", tool.name));
        }

        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .ok_or_else(|| eyre!("no scripted stream remaining"))?;

        Ok(Box::pin(stream::iter(script.into_iter().map(
            |chunk| match chunk.kind {
                ScriptedChunkKind::Reasoning(payload) => {
                    Ok(kraai_provider_core::ProviderStreamEvent::Reasoning { payload })
                }
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
                ScriptedChunkKind::ErrorWithSource(error, source) => {
                    Err(eyre!(source).wrap_err(error))
                }
                ScriptedChunkKind::Interrupted(reason) => {
                    Err(kraai_provider_core::ProviderError::StreamInterrupted(reason).into())
                }
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

pub(super) fn mock_model() -> kraai_provider_core::Model {
    kraai_provider_core::Model {
        options: Default::default(),
        supports_images: true,
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
        let deadline = tokio::time::Instant::now() + kraai_test_support::HANG_TIMEOUT;
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
    owns_directory: bool,
    pub(super) maintenance_tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl RuntimeTestHarness {
    pub(super) async fn new(scripts: Vec<Vec<ScriptedChunk>>) -> Option<Self> {
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock"),
            Box::new(ScriptedProvider {
                id: ProviderId::new("mock"),
                scripts: StdMutex::new(scripts.into()),
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
            }),
        );
        Self::new_with_parts(providers).await
    }

    pub(super) async fn new_with_parts(providers: ProviderManager) -> Option<Self> {
        Self::new_with_message_store(providers, None).await
    }

    pub(super) async fn new_with_message_store(
        providers: ProviderManager,
        message_store: Option<Arc<dyn kraai_persistence::MessageStore>>,
    ) -> Option<Self> {
        Self::new_in_directory(providers, message_store, None).await
    }

    pub(super) async fn new_shared(
        scripts: Vec<Vec<ScriptedChunk>>,
        data_dir: PathBuf,
    ) -> Option<Self> {
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock"),
            Box::new(ScriptedProvider {
                id: ProviderId::new("mock"),
                scripts: StdMutex::new(scripts.into()),
            }),
        );
        Self::new_in_directory(providers, None, Some(data_dir)).await
    }

    pub(super) async fn new_in_directory(
        providers: ProviderManager,
        message_store: Option<Arc<dyn kraai_persistence::MessageStore>>,
        shared_directory: Option<PathBuf>,
    ) -> Option<Self> {
        let owns_directory = shared_directory.is_none();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let data_dir = shared_directory.unwrap_or_else(|| {
            std::env::temp_dir().join(format!("kraai-runtime-test-{}-{nanos}", std::process::id()))
        });
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

        let message_store =
            message_store.unwrap_or_else(|| Arc::new(SqliteMessageStore::new(&data_dir)));
        let persistence =
            kraai_persistence::Persistence::open_with_messages(&data_dir, message_store)
                .await
                .expect("initialize persistence");
        let session_store = persistence.sessions().clone();
        let execution_store = persistence.executions().clone();
        let context_state_store = persistence.context().clone();
        let image_store = persistence.images().clone();
        let model_lists = Arc::new(super::super::model_lists::ModelListSync::new(
            persistence.model_lists().clone(),
        ));
        let (model_catalog_tx, _) =
            tokio::sync::watch::channel(providers.subscribe_model_catalog());
        let agent_manager = Arc::new(AgentManager::new(
            providers,
            data_dir.join("workspace"),
            persistence,
            data_dir.clone(),
        ));

        let setup = match kraai_provider_core::ProviderSet::load(
            &kraai_providers::DefaultProviders,
            &data_dir,
        )
        .await
        {
            Ok(setup) => setup,
            Err(error) if is_missing_system_ca_error(error.as_ref()) => return None,
            Err(error) => panic!("unexpected provider setup error: {error:?}"),
        };
        let events = EventCollector::default();
        let (command_tx, command_rx) = mpsc::channel(32);
        let event_tx = RuntimeEventSender::new(1024);
        let (startup_tx, startup_rx) =
            tokio::sync::watch::channel(crate::RuntimeStartupState::ready());
        let handle = RuntimeHandle {
            command_tx: command_tx.clone(),
            event_tx: event_tx.clone(),
            lifecycle: None,
            startup_rx,
        };
        let runtime = RuntimeCore {
            observed_sessions: Arc::default(),
            session_store,
            image_store,
            sessions: Arc::default(),
            event_tx: event_tx.clone(),
            command_tx,
            agent_manager,
            execution_store,
            context_state_store,
            provider_registry: setup.registry,
            stream_tasks: Default::default(),
            config_reload: Arc::default(),
            stopping: Arc::default(),
            provider_auth: Arc::new(setup.auth),
            config: Arc::new(RuntimeConfig {
                provider_config_path: data_dir.join("providers.toml"),
                use_current_executable_as_nushell_host: false,
                nushell_host_path: None,
                script_runtime_roots: None,
                script_output_root: data_dir.join("script-output"),
            }),
            startup_tx,
            model_catalog_tx,
            model_lists,
        };

        let events_for_task = events.clone();
        let mut event_rx = event_tx.subscribe();
        let event_task = tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(event) => events_for_task.on_event(event.event),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let runtime_for_task = runtime.clone();
        let runtime_task = tokio::spawn(async move {
            let (_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
            runtime_for_task.serve(command_rx, shutdown_rx).await;
        });

        let maintenance_tasks = vec![
            runtime.spawn_lease_heartbeat(),
            runtime.spawn_session_observers(),
            runtime.spawn_turn_timer_persistence(),
        ];
        Some(Self {
            owns_directory,
            maintenance_tasks,
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
        for task in self.maintenance_tasks {
            task.abort();
            let _ = task.await;
        }
        if self.owns_directory {
            let _ = tokio::fs::remove_dir_all(self.data_dir).await;
        }
    }
}

pub(super) async fn create_session_with_profile(
    handle: &RuntimeHandle,
    profile_id: &str,
) -> Result<SessionId> {
    let session_id = handle.create_session().await?;
    handle
        .set_session_profile(session_id.clone(), profile_id.to_string())
        .await?;
    Ok(session_id)
}

pub(super) fn queued(
    content: kraai_types::MessageContent,
    model_id: ModelId,
    provider_id: ProviderId,
) -> kraai_persistence::QueuedMessage {
    kraai_persistence::QueuedMessage {
        content,
        selection: kraai_types::ModelSelection {
            provider_id,
            model_id,
            options: Default::default(),
        },
    }
}

async fn with_turn<T>(
    runtime: &RuntimeCore,
    session: &SessionId,
    operation: impl AsyncFnOnce(&kraai_persistence::SqliteSessionStore) -> T,
) -> T {
    let store = &runtime.session_store;
    let claimed = !store.owns_turn(session).await.expect("session ownership");
    if claimed {
        store.claim_turn(session).await.expect("claim session");
    }
    let result = operation(store).await;
    if claimed {
        store.release_turn(session).await.expect("release session");
    }
    result
}

/// Queues messages as this runtime would, claiming the session only for the write.
pub(super) async fn enqueue(
    runtime: &RuntimeCore,
    session: &SessionId,
    messages: impl IntoIterator<Item = kraai_persistence::QueuedMessage>,
) {
    with_turn(runtime, session, async |store| {
        for message in messages {
            store
                .enqueue_message(session, &message)
                .await
                .expect("enqueue message");
        }
    })
    .await;
}

pub(super) async fn queue(
    runtime: &RuntimeCore,
    session: &SessionId,
) -> Vec<kraai_persistence::QueuedMessage> {
    runtime
        .session_store
        .queued_messages(session)
        .await
        .expect("queued messages")
        .into_iter()
        .map(|entry| entry.message)
        .collect()
}

pub(super) async fn take_queue(
    runtime: &RuntimeCore,
    session: &SessionId,
) -> Vec<kraai_persistence::QueuedMessage> {
    let messages = queue(runtime, session).await;
    with_turn(runtime, session, async |store| {
        store
            .clear_queued_messages(session)
            .await
            .expect("clear queue");
    })
    .await;
    messages
}

/// Takes the session's pending approval out of its actor, as a stopped runtime would lose it.
pub(super) async fn take_pending_approval(
    runtime: &RuntimeCore,
    session: &SessionId,
) -> super::super::script_execution::PendingScriptApproval {
    runtime
        .with_session_actor(session, |actor| {
            Box::pin(async move { actor.take_pending_approval() })
        })
        .await
        .expect("pending script approval")
}

/// Cancels the session's work the way the lease heartbeat does after losing `token`.
pub(super) async fn cancel_if_lease_matches(
    runtime: &RuntimeCore,
    session: &SessionId,
    token: i64,
) -> crate::RuntimeResult<bool> {
    let (response, cancelled) = tokio::sync::oneshot::channel();
    runtime.sessions.send(
        runtime,
        session,
        super::super::session::SessionMessage::Cancel {
            discard_queue: true,
            expected_lease: Some(token),
            response: Some(response),
        },
    );
    cancelled
        .await
        .expect("session actor answered the cancellation")
}

/// The message id of the latest stream started in `session`.
pub(super) fn started_stream(events: &[Event], session: &SessionId) -> kraai_types::MessageId {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::StreamStart {
                session_id,
                message_id,
            } if session_id == session => Some(kraai_types::MessageId::new(message_id.clone())),
            _ => None,
        })
        .expect("stream start event")
}

/// Holds the session in its preparing state until `release_preparation`.
pub(super) async fn hold_preparation(runtime: &RuntimeCore, session: &SessionId) {
    runtime
        .with_session_actor(session, |actor| {
            Box::pin(async move { actor.begin_test_preparation() })
        })
        .await;
}

pub(super) async fn release_preparation(runtime: &RuntimeCore, session: &SessionId) {
    runtime
        .with_session_actor(session, |actor| {
            Box::pin(async move { actor.end_test_preparation() })
        })
        .await;
}

/// Drives one provider response, applying its events the way the session actor does.
pub(super) async fn drive_stream(
    runtime: RuntimeCore,
    session: SessionId,
    providers: ProviderManager,
    request: kraai_agent::PendingStreamRequest,
) -> super::super::stream_driver::StreamDriveResult {
    let message_id = request.message_id.clone();
    let (mailbox, mut events) = mpsc::unbounded_channel();
    let reader = super::super::stream_driver::StreamReader {
        session_id: session.clone(),
        providers,
        agent_manager: runtime.agent_manager.clone(),
        session_store: runtime.session_store.clone(),
        event_tx: runtime.event_tx.clone(),
        barrier: runtime.sessions.barrier(&session),
        auxiliary_usage_task: runtime.stream_tasks.auxiliary_token(&session),
        mailbox,
        generation: 0,
    };
    let drive = reader.drive(request);
    tokio::pin!(drive);
    loop {
        tokio::select! {
            result = &mut drive => {
                while let Ok(message) = events.try_recv() {
                    if let super::super::session::SessionMessage::Stream { event, .. } = message {
                        super::super::session::apply_stream_event(&runtime, &session, &message_id, event).await;
                    }
                }
                return result;
            }
            Some(message) = events.recv() => {
                if let super::super::session::SessionMessage::Stream { event, .. } = message {
                    super::super::session::apply_stream_event(&runtime, &session, &message_id, event).await;
                }
            }
        }
    }
}

impl RuntimeCore {
    /// Runs `hook` inside the session's actor and returns its result.
    pub(in crate::runtime) async fn with_session_actor<T: Send + 'static>(
        &self,
        session_id: &kraai_types::SessionId,
        hook: impl for<'a> FnOnce(
            &'a mut super::super::session::SessionActor,
        ) -> futures::future::BoxFuture<'a, T>
        + Send
        + Sync
        + 'static,
    ) -> T {
        let (response, result) = tokio::sync::oneshot::channel();
        self.sessions.send(
            self,
            session_id,
            super::super::session::SessionMessage::Hook(Box::new(move |actor| {
                Box::pin(async move {
                    let _ = response.send(hook(actor).await);
                })
            })),
        );
        result.await.expect("session actor ran the hook")
    }
}

/// The assistant text an event streamed for `session`.
pub(super) fn streamed_text<'a>(event: &'a Event, session: &SessionId) -> Option<&'a str> {
    match event {
        Event::ItemStarted {
            session_id,
            item: kraai_types::AssistantItem::Text { text, .. },
            ..
        }
        | Event::ItemDelta {
            session_id,
            delta: text,
            ..
        } if session_id == session => Some(text),
        _ => None,
    }
}

/// Reads a session's whole history page by page, as a client would.
pub(super) trait FullHistory {
    async fn get_chat_history(
        &self,
        session_id: SessionId,
    ) -> crate::RuntimeResult<
        std::collections::BTreeMap<kraai_types::MessageId, kraai_types::Message>,
    >;
}

impl FullHistory for RuntimeHandle {
    async fn get_chat_history(
        &self,
        session_id: SessionId,
    ) -> crate::RuntimeResult<
        std::collections::BTreeMap<kraai_types::MessageId, kraai_types::Message>,
    > {
        let mut history = std::collections::BTreeMap::new();
        let mut from = None;
        loop {
            let page = self.get_history_page(session_id.clone(), from, 2).await?;
            history.extend(page.messages);
            match page.older_history {
                Some(older) => from = Some(older),
                None => return Ok(history),
            }
        }
    }
}
