use super::*;
use color_eyre::eyre::ensure;
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderRequestContext, ProviderStreamEvent,
};
use kraai_types::MessageId;
use kraai_types::{MessageStatus, TokenUsage, ToolCallId};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Summarizer {
    calls: Arc<AtomicUsize>,
    response: String,
    delay: std::time::Duration,
}

#[async_trait::async_trait]
impl Provider for Summarizer {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("test")
    }
    async fn list_models(&self) -> Vec<Model> {
        Vec::new()
    }
    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }
    async fn register_model(&mut self, _: ModelConfig) -> Result<()> {
        Ok(())
    }
    async fn generate_reply_stream(
        &self,
        _: &ModelId,
        request: ProviderRequest,
        _: &ProviderRequestContext,
    ) -> Result<futures::stream::BoxStream<'static, Result<ProviderStreamEvent>>> {
        ensure!(request.script_tool.is_none());
        let source = serde_json::to_string(&request.messages)?;
        ensure!(!source.contains("system-only-marker"));
        ensure!(!source.contains("pinned-only-marker"));
        ensure!(!source.contains("encrypted-only-marker"));
        ensure!(estimate_request(&request) < input_limit(16384));
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let mut events = vec![Ok(ProviderStreamEvent::TextDelta {
            item_id: "summary".into(),
            phase: AssistantPhase::FinalAnswer,
            delta: self.response.clone(),
        })];
        if self.delay.is_zero() {
            events.push(Ok(ProviderStreamEvent::Usage(TokenUsage {
                input_tokens: 100,
                output_tokens: 10,
                ..Default::default()
            })));
        }
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

fn message(id: &str, content: ConversationItem) -> Message {
    Message {
        id: MessageId::new(id),
        parent_id: None,
        content,
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    }
}

fn text(id: &str, content: &str) -> Message {
    message(
        id,
        ConversationItem::Assistant {
            items: vec![AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: content.into(),
            }],
        },
    )
}

fn fixture(history: Vec<Message>) -> (ContextCompaction, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("compaction-agent-{}", ulid::Ulid::generate()));
    let original = assemble(
        "system-only-marker",
        "pinned-only-marker",
        None,
        &history,
        None,
    );
    (
        ContextCompaction {
            store: FileCompactionStore::new(&root),
            usage_store: Arc::new(kraai_persistence::FileRequestUsageStore::new(&root)),
            session_id: "session".into(),
            prefix: "system-only-marker".into(),
            suffix: "pinned-only-marker".into(),
            original,
            history,
            previous: None,
            pinned_user: None,
            max_context: 16384,
            used_context_tokens: 14000,
            on_usage: None,
            usage_barrier: None,
        },
        root,
    )
}

fn provider(response: &str) -> (ProviderManager, Arc<AtomicUsize>) {
    provider_with_delay(response, std::time::Duration::ZERO)
}

fn provider_with_delay(
    response: &str,
    delay: std::time::Duration,
) -> (ProviderManager, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut manager = ProviderManager::new();
    manager.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            calls: calls.clone(),
            response: response.into(),
            delay,
        }),
    );
    (manager, calls)
}

#[tokio::test]
async fn compacts_oversized_history_in_chunks_and_records_usage() -> Result<()> {
    let mut history = vec![
        text("old", &"old detail ".repeat(12000)),
        message(
            "request",
            ConversationItem::User {
                text: "Fix the parser".into(),
            },
        ),
        text("recent", "Investigating"),
    ];
    let reasoning = AssistantItem::Reasoning {
        provider_id: ProviderId::new("test"),
        payload: serde_json::json!({"type":"reasoning","id":"rs-1","encrypted_content":"encrypted-only-marker","summary":[]}),
    };
    for message in &mut history {
        if let ConversationItem::Assistant { items } = &mut message.content {
            items.insert(0, reasoning.clone());
        }
    }
    history
        .last_mut()
        .ok_or_else(|| eyre!("Missing recent message"))?
        .generation = Some(kraai_types::MessageGeneration {
        provider_id: ProviderId::new("test"),
        model_id: ModelId::new("test"),
        max_context: Some(16384),
        usage: Some(TokenUsage {
            input_tokens: 14000,
            ..Default::default()
        }),
    });
    let (context, root) = fixture(history);
    let (providers, calls) = provider("The user wants the parser fixed. Investigation is ongoing.");
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("test"))
        .await?;
    ensure!(outcome.compacted);
    ensure!(outcome.request.messages.iter().any(|message| matches!(message, ConversationItem::Assistant { items } if items.contains(&reasoning))));
    ensure!(outcome.request.messages.first() == context.original.messages.first());
    ensure!(outcome.request.messages.last() == context.original.messages.last());
    ensure!(outcome.request.cacheable_messages == Some(outcome.request.messages.len() - 1));
    ensure!(calls.load(Ordering::SeqCst) > 1);
    ensure!(
        outcome
            .requests
            .iter()
            .all(|request| request.usage.is_some())
    );
    ensure!(context.usage_store.load("session").await?.len() == calls.load(Ordering::SeqCst));
    ensure!(estimate_request(&outcome.request) < input_limit(16384) * 30 / 100);
    ensure!(
        outcome.request.messages.iter().any(
            |item| matches!(item, ConversationItem::User { text } if text == "Fix the parser")
        )
    );
    let checkpoint = context
        .store
        .get(&MessageId::new("old"))
        .await?
        .ok_or_else(|| eyre!("Missing checkpoint"))?;
    ensure!(checkpoint.superseded_usage == vec![MessageId::new("recent")]);
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[test]
fn boundary_keeps_calls_and_results_together_and_preserves_old_user() -> Result<()> {
    let call_id = ToolCallId::new("call");
    let history = vec![
        message(
            "user",
            ConversationItem::User {
                text: "Do not change the public API".into(),
            },
        ),
        message(
            "call",
            ConversationItem::Assistant {
                items: vec![AssistantItem::ScriptCall {
                    call_id: call_id.clone(),
                    name: "kraai_nushell".into(),
                    input: "x".repeat(36000),
                }],
            },
        ),
        message(
            "result",
            ConversationItem::ScriptResult {
                call_id,
                output: "done".into(),
            },
        ),
        text("tail", "Working"),
    ];
    let (context, _) = fixture(history);
    let (cut, _, pinned) = context.plan()?;
    ensure!(cut == 3);
    ensure!(
        matches!(pinned, Some(ConversationItem::User { ref text }) if text == "Do not change the public API")
    );
    Ok(())
}

#[tokio::test]
async fn invalid_summary_falls_back_only_when_original_fits_and_never_persists() -> Result<()> {
    let (context, root) = fixture(vec![
        text("old", &"x".repeat(90000)),
        text("tail", "recent"),
    ]);
    let (providers, _) = provider("");
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("test"))
        .await?;
    ensure!(!outcome.compacted);
    ensure!(context.store.get(&MessageId::new("old")).await?.is_none());
    ensure!(estimate_request(&outcome.request) == estimate_request(&context.original));
    let (mut oversized, oversized_root) = fixture(vec![
        text("old", &"x".repeat(10000)),
        text("tail", "recent"),
    ]);
    oversized.used_context_tokens = oversized.max_context;
    ensure!(
        oversized
            .run(&providers, &ProviderId::new("test"), &ModelId::new("test"))
            .await
            .is_err()
    );
    ensure!(oversized.store.get(&MessageId::new("old")).await?.is_none());
    tokio::fs::remove_dir_all(root).await?;
    tokio::fs::remove_dir_all(oversized_root).await?;
    Ok(())
}

#[test]
fn fixed_context_and_unresolved_call_cannot_be_compacted_away() -> Result<()> {
    let (mut context, _) = fixture(vec![
        text("old", &"x".repeat(36000)),
        text("tail", "recent"),
    ]);
    context.prefix = "x".repeat(60000);
    ensure!(context.plan().is_err());
    context.prefix.clear();
    context.history.insert(
        0,
        message(
            "call",
            ConversationItem::Assistant {
                items: vec![AssistantItem::ScriptCall {
                    call_id: ToolCallId::new("missing-result"),
                    name: "tool".into(),
                    input: "input".into(),
                }],
            },
        ),
    );
    ensure!(context.plan().is_err());
    Ok(())
}

#[test]
fn unicode_estimation_and_chunking_do_not_split_characters() -> Result<()> {
    ensure!(estimate_text("😀") == 4);
    let source = "a😀中".repeat(100);
    let end = super::summary::chunk_end(&source, 30);
    ensure!(source.is_char_boundary(end));
    ensure!(estimate_text(source.get(..end).ok_or_else(|| eyre!("bad boundary"))?) <= 30);
    Ok(())
}

#[tokio::test]
async fn giant_final_tool_result_can_be_fully_compacted_with_user_preserved() -> Result<()> {
    let call_id = ToolCallId::new("last-call");
    let history = vec![
        message(
            "user",
            ConversationItem::User {
                text: "Inspect the failing parser".into(),
            },
        ),
        message(
            "call",
            ConversationItem::Assistant {
                items: vec![AssistantItem::ScriptCall {
                    call_id: call_id.clone(),
                    name: "kraai_nushell".into(),
                    input: "inspect".into(),
                }],
            },
        ),
        message(
            "result",
            ConversationItem::ScriptResult {
                call_id,
                output: "output".repeat(10000),
            },
        ),
    ];
    let (context, root) = fixture(history);
    let (providers, _) =
        provider("Inspection found a parser error; the user requested investigation.");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    let context = context.observe_usage(
        Arc::new(tokio::sync::RwLock::new(())),
        Arc::new(move |request| {
            if let Ok(mut events) = captured.lock() {
                events.push(request);
            }
        }),
    );
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("test"))
        .await?;
    ensure!(outcome.compacted);
    ensure!(
        context
            .store
            .get(&MessageId::new("result"))
            .await?
            .is_some()
    );
    ensure!(outcome.request.messages.iter().any(|item| matches!(item, ConversationItem::User { text } if text == "Inspect the failing parser")));
    let count = events
        .lock()
        .map_err(|error| eyre!("poisoned events: {error}"))?
        .len();
    ensure!(count == outcome.requests.len() * 2);
    ensure!(
        outcome
            .requests
            .iter()
            .all(|request| request.started_at > 1_000_000_000_000)
    );
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn successful_chunks_share_one_overall_deadline() -> Result<()> {
    let (mut context, root) = fixture(vec![
        text("old", &"old detail ".repeat(12000)),
        text("recent", "Investigating"),
    ]);
    context.used_context_tokens = context.max_context;
    let (providers, calls) = provider_with_delay(
        "Investigation continues.",
        std::time::Duration::from_secs(110),
    );
    let started = tokio::time::Instant::now();
    let error = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("test"))
        .await
        .err()
        .ok_or_else(|| eyre!("Expected overall timeout"))?;
    ensure!(format!("{error:#}").contains("Context summarization timed out"));
    ensure!(started.elapsed() == std::time::Duration::from_secs(600));
    ensure!(calls.load(Ordering::SeqCst) == 6);
    ensure!(context.usage_store.load("session").await?.len() == 6);
    ensure!(context.store.get(&MessageId::new("old")).await?.is_none());
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn per_call_timeout_still_falls_back_when_original_fits() -> Result<()> {
    let (context, root) = fixture(vec![
        text("old", &"x".repeat(10000)),
        text("recent", "Investigating"),
    ]);
    let (providers, calls) = provider_with_delay(
        "Investigation continues.",
        std::time::Duration::from_secs(121),
    );
    let started = tokio::time::Instant::now();
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("test"))
        .await?;
    ensure!(!outcome.compacted);
    ensure!(
        outcome
            .notification
            .contains("Context summarization timed out")
    );
    ensure!(started.elapsed() == std::time::Duration::from_secs(120));
    ensure!(calls.load(Ordering::SeqCst) == 1);
    ensure!(outcome.request.messages == context.original.messages);
    ensure!(context.store.get(&MessageId::new("old")).await?.is_none());
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}
