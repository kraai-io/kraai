use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use color_eyre::eyre::{Result, eyre};
use futures::stream::{self, BoxStream};
use kraai_agent::AgentManager;
use kraai_persistence::{
    FileMessageStore, FileSessionStore, MessageStore, SessionMeta, SessionStore,
};
use kraai_provider_core::{ModelConfig, ProviderManager};
use kraai_tool_core::{ToolCallResult, ToolContext, ToolManager, TypedTool};
use kraai_types::{
    ChatMessage, ChatRole, ExecutionPolicy, MessageStatus, ModelId, ProviderId, RiskLevel,
    TokenUsage, ToolCallAssessment,
};
use serde::Deserialize;
use tokio::sync::{Mutex, broadcast, mpsc};

use super::super::builder::build_provider_registry;
use super::super::core::RuntimeCore;
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
    Text(String),
    Usage(TokenUsage),
}

#[derive(Clone, Debug)]
pub(super) struct ScriptedChunk {
    kind: ScriptedChunkKind,
    gate: Option<Arc<tokio::sync::Notify>>,
}

impl ScriptedChunk {
    pub(super) fn plain(text: impl Into<String>) -> Self {
        Self {
            kind: ScriptedChunkKind::Text(text.into()),
            gate: None,
        }
    }

    pub(super) fn gated(text: impl Into<String>, gate: Arc<tokio::sync::Notify>) -> Self {
        Self {
            kind: ScriptedChunkKind::Text(text.into()),
            gate: Some(gate),
        }
    }

    pub(super) fn usage(usage: TokenUsage) -> Self {
        Self {
            kind: ScriptedChunkKind::Usage(usage),
            gate: None,
        }
    }
}

#[derive(Clone, Deserialize)]
pub(super) struct ValueArgs {
    value: String,
}

#[derive(Clone, Deserialize)]
struct NoopArgs {}

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
        vec![kraai_provider_core::Model {
            id: ModelId::new("mock-model"),
            name: String::from("Mock Model"),
            max_context: None,
        }]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<ChatMessage> {
        Ok(ChatMessage {
            role: ChatRole::Assistant,
            content: String::from("unused non-streaming reply"),
        })
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .ok_or_else(|| eyre!("No scripted stream remaining"))?;

        Ok(Box::pin(stream::unfold(
            (script, 0usize),
            |(script, index)| async move {
                if index >= script.len() {
                    return None;
                }

                let chunk = script[index].clone();
                if let Some(gate) = chunk.gate {
                    gate.notified().await;
                }

                let event = match chunk.kind {
                    ScriptedChunkKind::Text(text) => {
                        kraai_provider_core::ProviderStreamEvent::TextDelta(text)
                    }
                    ScriptedChunkKind::Usage(usage) => {
                        kraai_provider_core::ProviderStreamEvent::Usage(usage)
                    }
                };

                Some((Ok(event), (script, index + 1)))
            },
        )))
    }
}

#[derive(Clone, Copy)]
pub(super) struct ApprovalTool;

#[async_trait]
impl TypedTool for ApprovalTool {
    type Args = ValueArgs;

    fn name(&self) -> &'static str {
        "mock_tool"
    }

    fn schema(&self) -> &'static str {
        "mock_tool(value: string)"
    }

    fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::UndoableWorkspaceWrite,
            policy: ExecutionPolicy::AlwaysAsk,
            reasons: vec![format!("mock_tool requires approval for {:?}", args.value)],
        }
    }

    async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
        ToolCallResult::success(serde_json::json!({
            "tool": "mock_tool",
            "value": args.value,
        }))
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Mock tool for {}", args.value)
    }
}

#[derive(Clone, Copy)]
pub(super) struct AutonomousTool;

#[async_trait]
impl TypedTool for AutonomousTool {
    type Args = ValueArgs;

    fn name(&self) -> &'static str {
        "auto_tool"
    }

    fn schema(&self) -> &'static str {
        "auto_tool(value: string)"
    }

    fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::ReadOnlyWorkspace,
            policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
            reasons: vec![format!(
                "auto_tool can run autonomously for {:?}",
                args.value
            )],
        }
    }

    async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
        ToolCallResult::success(serde_json::json!({
            "tool": "auto_tool",
            "value": args.value,
        }))
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Autonomous tool for {}", args.value)
    }
}

#[derive(Clone)]
pub(super) struct BlockingApprovalTool {
    pub(super) started: Arc<tokio::sync::Notify>,
    pub(super) release: Arc<tokio::sync::Notify>,
    pub(super) fail_message: Option<String>,
}

#[async_trait]
impl TypedTool for BlockingApprovalTool {
    type Args = ValueArgs;

    fn name(&self) -> &'static str {
        "blocking_tool"
    }

    fn schema(&self) -> &'static str {
        "blocking_tool(value: string)"
    }

    fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::UndoableWorkspaceWrite,
            policy: ExecutionPolicy::AlwaysAsk,
            reasons: vec![format!(
                "blocking_tool requires approval for {:?}",
                args.value
            )],
        }
    }

    async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
        self.started.notify_waiters();
        self.release.notified().await;

        if let Some(message) = &self.fail_message {
            ToolCallResult::error(message.clone())
        } else {
            ToolCallResult::success(serde_json::json!({
                "tool": "blocking_tool",
                "value": args.value,
            }))
        }
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Blocking tool for {}", args.value)
    }
}

#[derive(Clone)]
pub(super) struct BatchBlockingApprovalTool {
    pub(super) started: Arc<tokio::sync::Notify>,
    pub(super) ready: Arc<tokio::sync::Barrier>,
}

#[async_trait]
impl TypedTool for BatchBlockingApprovalTool {
    type Args = ValueArgs;

    fn name(&self) -> &'static str {
        "batch_blocking_tool"
    }

    fn schema(&self) -> &'static str {
        "batch_blocking_tool(value: string)"
    }

    fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::UndoableWorkspaceWrite,
            policy: ExecutionPolicy::AlwaysAsk,
            reasons: vec![format!(
                "batch_blocking_tool requires approval for {:?}",
                args.value
            )],
        }
    }

    async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
        self.started.notify_waiters();
        self.ready.wait().await;
        if args.value == "beta" {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        ToolCallResult::success(serde_json::json!({
            "tool": "batch_blocking_tool",
            "value": args.value,
        }))
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Batch blocking tool for {}", args.value)
    }
}

#[derive(Clone, Copy)]
pub(super) struct FailingApprovalTool;

#[async_trait]
impl TypedTool for FailingApprovalTool {
    type Args = ValueArgs;

    fn name(&self) -> &'static str {
        "failing_tool"
    }

    fn schema(&self) -> &'static str {
        "failing_tool(value: string)"
    }

    fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::UndoableWorkspaceWrite,
            policy: ExecutionPolicy::AlwaysAsk,
            reasons: vec![format!(
                "failing_tool requires approval for {:?}",
                args.value
            )],
        }
    }

    async fn call(&self, _args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
        ToolCallResult::error(String::from("tool exploded"))
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Failing tool for {}", args.value)
    }
}

pub(super) struct BlockingStartProvider {
    pub(super) id: ProviderId,
    pub(super) started: Arc<tokio::sync::Notify>,
    pub(super) release: Arc<tokio::sync::Notify>,
}

pub(super) struct DeferredFailingProvider {
    pub(super) id: ProviderId,
    pub(super) started: Arc<tokio::sync::Notify>,
    pub(super) release: Arc<tokio::sync::Notify>,
    pub(super) failure_message: String,
}

pub(super) struct RetryNotifyingProvider {
    pub(super) id: ProviderId,
}

#[async_trait]
impl kraai_provider_core::Provider for BlockingStartProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![kraai_provider_core::Model {
            id: ModelId::new("mock-model"),
            name: String::from("Mock Model"),
            max_context: None,
        }]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<ChatMessage> {
        Ok(ChatMessage {
            role: ChatRole::Assistant,
            content: String::from("unused non-streaming reply"),
        })
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        self.started.notify_waiters();
        self.release.notified().await;
        Ok(Box::pin(stream::once(async {
            Ok(kraai_provider_core::ProviderStreamEvent::TextDelta(
                String::from("provider started"),
            ))
        })))
    }
}

#[async_trait]
impl kraai_provider_core::Provider for DeferredFailingProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![kraai_provider_core::Model {
            id: ModelId::new("mock-model"),
            name: String::from("Mock Model"),
            max_context: None,
        }]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<ChatMessage> {
        Ok(ChatMessage {
            role: ChatRole::Assistant,
            content: String::from("unused non-streaming reply"),
        })
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
        _request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        self.started.notify_waiters();
        self.release.notified().await;
        Err(eyre!(self.failure_message.clone()))
    }
}

#[async_trait]
impl kraai_provider_core::Provider for RetryNotifyingProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![kraai_provider_core::Model {
            id: ModelId::new("mock-model"),
            name: String::from("Mock Model"),
            max_context: None,
        }]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
        request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<ChatMessage> {
        if let Some(observer) = request_context.retry_observer() {
            observer.on_retry_scheduled(&kraai_provider_core::ProviderRetryEvent {
                operation: "responses",
                retry_number: 1,
                delay: Duration::from_secs(1),
                reason: String::from("HTTP 429"),
            });
        }

        Ok(ChatMessage {
            role: ChatRole::Assistant,
            content: String::from("unused non-streaming reply"),
        })
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        _messages: Vec<ChatMessage>,
        request_context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>> {
        if let Some(observer) = request_context.retry_observer() {
            observer.on_retry_scheduled(&kraai_provider_core::ProviderRetryEvent {
                operation: "responses",
                retry_number: 1,
                delay: Duration::from_secs(1),
                reason: String::from("HTTP 429"),
            });
        }

        Ok(Box::pin(stream::once(async {
            Ok(kraai_provider_core::ProviderStreamEvent::TextDelta(
                String::from("provider started"),
            ))
        })))
    }
}

#[derive(Clone, Copy)]
struct NoopTool;

#[async_trait]
impl TypedTool for NoopTool {
    type Args = NoopArgs;

    fn name(&self) -> &'static str {
        "noop_tool"
    }

    fn schema(&self) -> &'static str {
        "noop_tool()"
    }

    async fn call(&self, _args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
        ToolCallResult::success(serde_json::json!({ "ok": true }))
    }
}

#[derive(Clone, Default)]
pub(super) struct EventCollector {
    events: Arc<StdMutex<Vec<Event>>>,
}

pub(super) struct FailOnAssistantCompletionMessageStore {
    pub(super) inner: Arc<dyn MessageStore>,
    pub(super) should_fail: Arc<AtomicBool>,
}

pub(super) struct FailOnToolMessageStore {
    pub(super) inner: Arc<dyn MessageStore>,
    pub(super) should_fail: Arc<AtomicBool>,
}

pub(super) struct FailOnDemandSessionStore {
    pub(super) inner: Arc<dyn SessionStore>,
    pub(super) should_fail: Arc<AtomicBool>,
}

#[async_trait]
impl MessageStore for FailOnAssistantCompletionMessageStore {
    async fn get(&self, id: &kraai_types::MessageId) -> Result<Option<kraai_types::Message>> {
        self.inner.get(id).await
    }

    async fn save(&self, message: &kraai_types::Message) -> Result<()> {
        if self.should_fail.load(Ordering::SeqCst)
            && message.role == ChatRole::Assistant
            && message.status == MessageStatus::Complete
        {
            return Err(eyre!("intentional assistant completion save failure"));
        }

        self.inner.save(message).await
    }

    async fn unload(&self, id: &kraai_types::MessageId) {
        self.inner.unload(id).await;
    }

    async fn delete(&self, id: &kraai_types::MessageId) -> Result<()> {
        self.inner.delete(id).await
    }

    async fn exists(&self, id: &kraai_types::MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }

    async fn list_all_on_disk(&self) -> Result<std::collections::HashSet<kraai_types::MessageId>> {
        self.inner.list_all_on_disk().await
    }

    async fn list_hot(&self) -> Result<std::collections::HashSet<kraai_types::MessageId>> {
        self.inner.list_hot().await
    }
}

#[async_trait]
impl MessageStore for FailOnToolMessageStore {
    async fn get(&self, id: &kraai_types::MessageId) -> Result<Option<kraai_types::Message>> {
        self.inner.get(id).await
    }

    async fn save(&self, message: &kraai_types::Message) -> Result<()> {
        if self.should_fail.load(Ordering::SeqCst) && message.role == ChatRole::Tool {
            return Err(eyre!("intentional tool history save failure"));
        }

        self.inner.save(message).await
    }

    async fn unload(&self, id: &kraai_types::MessageId) {
        self.inner.unload(id).await;
    }

    async fn delete(&self, id: &kraai_types::MessageId) -> Result<()> {
        self.inner.delete(id).await
    }

    async fn exists(&self, id: &kraai_types::MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }

    async fn list_all_on_disk(&self) -> Result<std::collections::HashSet<kraai_types::MessageId>> {
        self.inner.list_all_on_disk().await
    }

    async fn list_hot(&self) -> Result<std::collections::HashSet<kraai_types::MessageId>> {
        self.inner.list_hot().await
    }
}

#[async_trait]
impl SessionStore for FailOnDemandSessionStore {
    async fn list(&self) -> Result<Vec<SessionMeta>> {
        self.inner.list().await
    }

    async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
        self.inner.get(id).await
    }

    async fn save(&self, session: &SessionMeta) -> Result<()> {
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(eyre!("intentional session save failure for {}", session.id));
        }

        self.inner.save(session).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = self.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "Timed out waiting for {description}. Events so far: {snapshot:#?}"
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
    runtime_task: tokio::task::JoinHandle<()>,
    event_task: tokio::task::JoinHandle<()>,
    pub(super) data_dir: PathBuf,
}

impl RuntimeTestHarness {
    pub(super) async fn new(scripts: Vec<Vec<ScriptedChunk>>) -> Option<Self> {
        Self::new_with_tools(scripts, |_| {}).await
    }

    pub(super) async fn new_with_tools<F>(
        scripts: Vec<Vec<ScriptedChunk>>,
        configure_tools: F,
    ) -> Option<Self>
    where
        F: FnOnce(&mut ToolManager),
    {
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock"),
            Box::new(ScriptedProvider {
                id: ProviderId::new("mock"),
                scripts: StdMutex::new(scripts.into()),
            }),
        );

        let mut tools = ToolManager::new();
        tools.register_tool(ApprovalTool);
        tools.register_tool(AutonomousTool);
        configure_tools(&mut tools);

        Self::new_with_parts(providers, tools).await
    }

    pub(super) async fn new_with_parts(
        providers: ProviderManager,
        mut tools: ToolManager,
    ) -> Option<Self> {
        if tools.list_tools().is_empty() {
            tools.register_tool(ApprovalTool);
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let data_dir = std::env::temp_dir().join(format!("kraai-runtime-test-{pid}-{nanos}"));
        tokio::fs::create_dir_all(&data_dir)
            .await
            .expect("create temp runtime dir");

        let workspace_dir = data_dir.join("workspace");
        tokio::fs::create_dir_all(&workspace_dir)
            .await
            .expect("create temp workspace dir");
        tools.register_tool(NoopTool);
        let profile_dir = workspace_dir.join(".kraai");
        tokio::fs::create_dir_all(&profile_dir)
            .await
            .expect("create temp profile dir");
        let tool_ids = tools
            .list_tools()
            .into_iter()
            .map(|tool_id| format!("\"{}\"", tool_id))
            .collect::<Vec<_>>()
            .join(", ");
        let profile_doc = format!(
            "[[profiles]]\n\
id = \"test-profile\"\n\
display_name = \"Test Profile\"\n\
description = \"Runtime test profile\"\n\
system_prompt = \"Runtime test profile\"\n\
tools = [{tool_ids}]\n\
default_risk_level = \"undoable_workspace_write\"\n"
        );
        tokio::fs::write(profile_dir.join("agents.toml"), profile_doc)
            .await
            .expect("write test profile config");

        let message_store = Arc::new(FileMessageStore::new(&data_dir));
        let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));

        Self::new_with_stores_and_parts(providers, tools, message_store, session_store, data_dir)
            .await
    }

    async fn new_with_stores_and_parts(
        providers: ProviderManager,
        mut tools: ToolManager,
        message_store: Arc<dyn MessageStore>,
        session_store: Arc<dyn SessionStore>,
        data_dir: PathBuf,
    ) -> Option<Self> {
        if tools.list_tools().is_empty() {
            tools.register_tool(ApprovalTool);
        }

        let agent_manager = Arc::new(Mutex::new(AgentManager::new(
            providers,
            tools,
            data_dir.join("workspace"),
            message_store,
            session_store,
        )));

        let events = EventCollector::default();
        let (command_tx, mut command_rx) = mpsc::channel(32);
        let (event_tx, _) = broadcast::channel(1024);
        let handle = RuntimeHandle {
            command_tx: command_tx.clone(),
            event_tx: event_tx.clone(),
        };

        let openai_codex_auth = match kraai_provider_openai_codex::OpenAiCodexAuthController::new()
        {
            Ok(controller) => Arc::new(controller),
            Err(error) if is_missing_system_ca_error(&error) => return None,
            Err(error) => panic!("unexpected openai auth controller init error: {error}"),
        };
        let runtime = RuntimeCore {
            event_tx: event_tx.clone(),
            command_tx,
            agent_manager,
            provider_registry: build_provider_registry(openai_codex_auth.clone())
                .expect("provider registry"),
            active_streams: Arc::new(Mutex::new(HashMap::new())),
            queued_messages: Arc::new(Mutex::new(HashMap::new())),
            openai_codex_auth,
            provider_config_path: data_dir.join("providers.toml"),
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

        let runtime_task = tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                runtime
                    .handle_command(command)
                    .await
                    .expect("runtime command should succeed");
            }
        });

        Some(Self {
            handle,
            events,
            runtime_task,
            event_task,
            data_dir,
        })
    }

    pub(super) async fn new_with_message_store<F>(
        scripts: Vec<Vec<ScriptedChunk>>,
        configure_store: F,
    ) -> Option<Self>
    where
        F: FnOnce(Arc<dyn MessageStore>) -> Arc<dyn MessageStore>,
    {
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("mock"),
            Box::new(ScriptedProvider {
                id: ProviderId::new("mock"),
                scripts: StdMutex::new(scripts.into()),
            }),
        );

        let mut tools = ToolManager::new();
        tools.register_tool(ApprovalTool);
        tools.register_tool(AutonomousTool);
        tools.register_tool(NoopTool);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let data_dir = std::env::temp_dir().join(format!("kraai-runtime-test-{pid}-{nanos}"));
        tokio::fs::create_dir_all(&data_dir)
            .await
            .expect("create temp runtime dir");
        let workspace_dir = data_dir.join("workspace");
        tokio::fs::create_dir_all(&workspace_dir)
            .await
            .expect("create temp workspace dir");
        let profile_dir = workspace_dir.join(".kraai");
        tokio::fs::create_dir_all(&profile_dir)
            .await
            .expect("create temp profile dir");
        let tool_ids = tools
            .list_tools()
            .into_iter()
            .map(|tool_id| format!("\"{}\"", tool_id))
            .collect::<Vec<_>>()
            .join(", ");
        let profile_doc = format!(
            "[[profiles]]\n\
id = \"test-profile\"\n\
display_name = \"Test Profile\"\n\
description = \"Runtime test profile\"\n\
system_prompt = \"Runtime test profile\"\n\
tools = [{tool_ids}]\n\
default_risk_level = \"undoable_workspace_write\"\n"
        );
        tokio::fs::write(profile_dir.join("agents.toml"), profile_doc)
            .await
            .expect("write test profile config");

        let base_store: Arc<dyn MessageStore> = Arc::new(FileMessageStore::new(&data_dir));
        let message_store = configure_store(base_store.clone());
        let session_store: Arc<dyn SessionStore> =
            Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));

        Self::new_with_stores_and_parts(providers, tools, message_store, session_store, data_dir)
            .await
    }

    pub(super) async fn new_with_provider_and_session_store<F>(
        provider: Box<dyn kraai_provider_core::Provider>,
        configure_session_store: F,
    ) -> Option<Self>
    where
        F: FnOnce(Arc<dyn SessionStore>) -> Arc<dyn SessionStore>,
    {
        let mut providers = ProviderManager::new();
        providers.register_provider(ProviderId::new("mock"), provider);

        let mut tools = ToolManager::new();
        tools.register_tool(ApprovalTool);
        tools.register_tool(AutonomousTool);
        tools.register_tool(NoopTool);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let data_dir = std::env::temp_dir().join(format!("kraai-runtime-test-{pid}-{nanos}"));
        tokio::fs::create_dir_all(&data_dir)
            .await
            .expect("create temp runtime dir");
        let workspace_dir = data_dir.join("workspace");
        tokio::fs::create_dir_all(&workspace_dir)
            .await
            .expect("create temp workspace dir");
        let profile_dir = workspace_dir.join(".kraai");
        tokio::fs::create_dir_all(&profile_dir)
            .await
            .expect("create temp profile dir");
        let tool_ids = tools
            .list_tools()
            .into_iter()
            .map(|tool_id| format!("\"{}\"", tool_id))
            .collect::<Vec<_>>()
            .join(", ");
        let profile_doc = format!(
            "[[profiles]]\n\
id = \"test-profile\"\n\
display_name = \"Test Profile\"\n\
description = \"Runtime test profile\"\n\
system_prompt = \"Runtime test profile\"\n\
tools = [{tool_ids}]\n\
default_risk_level = \"undoable_workspace_write\"\n"
        );
        tokio::fs::write(profile_dir.join("agents.toml"), profile_doc)
            .await
            .expect("write test profile config");

        let message_store: Arc<dyn MessageStore> = Arc::new(FileMessageStore::new(&data_dir));
        let base_session_store: Arc<dyn SessionStore> =
            Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
        let session_store = configure_session_store(base_session_store);

        Self::new_with_stores_and_parts(providers, tools, message_store, session_store, data_dir)
            .await
    }

    pub(super) async fn shutdown(self) {
        drop(self.handle);
        self.event_task.abort();
        self.runtime_task.abort();
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

pub(super) fn stream_complete_for(events: &[Event], session_id: &str) -> usize {
    events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::StreamComplete {
                    session_id: event_session,
                    ..
                } if event_session == session_id
            )
        })
        .expect("stream complete event should exist")
}

pub(super) fn stream_start_count(events: &[Event], session_id: &str) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::StreamStart {
                    session_id: event_session,
                    ..
                } if event_session == session_id
            )
        })
        .count()
}

pub(super) fn stream_complete_count(events: &[Event], session_id: &str) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::StreamComplete {
                    session_id: event_session,
                    ..
                } if event_session == session_id
            )
        })
        .count()
}

pub(super) fn continuation_failed_count(events: &[Event], session_id: &str) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::ContinuationFailed {
                    session_id: event_session,
                    ..
                } if event_session == session_id
            )
        })
        .count()
}

pub(super) fn call_id_for_queue_order(
    events: &[Event],
    session_id: &str,
    tool_id: &str,
    queue_order: u64,
) -> String {
    events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallDetected {
                session_id: event_session,
                call_id,
                tool_id: event_tool_id,
                queue_order: event_queue_order,
                ..
            } if event_session == session_id
                && event_tool_id == tool_id
                && *event_queue_order == queue_order =>
            {
                Some(call_id.clone())
            }
            _ => None,
        })
        .expect("tool call id should exist")
}
