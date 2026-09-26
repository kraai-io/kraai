use super::*;
use color_eyre::eyre::ensure;
use futures::stream::{self, BoxStream};
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderRequestContext, ProviderStreamEvent,
};
use kraai_types::{
    AssistantItem, AssistantPhase, MessageId, MessageStatus, TokenUsage, ToolCallId,
};
use std::sync::Mutex;

type Requests = Arc<Mutex<Vec<ProviderRequest>>>;

struct Summarizer {
    requests: Requests,
    native: bool,
    events: Vec<ProviderStreamEvent>,
    fail: bool,
    reject_large_input: bool,
    interruptions: usize,
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
    fn supports_native_compaction(&self, _: &ModelId) -> bool {
        self.native
    }
    async fn compact_stream(
        &self,
        _: &ModelId,
        request: ProviderRequest,
        _: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        ensure!(self.native);
        self.respond(request)
    }
    async fn generate_reply_stream(
        &self,
        _: &ModelId,
        request: ProviderRequest,
        _: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        ensure!(
            !self.native,
            "Native compaction must not use text summarization"
        );
        ensure!(request.script_tool.is_none());
        self.respond(request)
    }
}

impl Summarizer {
    fn respond(
        &self,
        request: ProviderRequest,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let too_large = self.reject_large_input && request.messages.iter().any(|item| matches!(item, ConversationItem::ScriptResult { output, .. } if output.len() > 1000));
        let attempt = {
            let mut requests = self.requests.lock().map_err(|e| eyre!("{e}"))?;
            requests.push(request);
            requests.len()
        };
        if too_large {
            return Err(kraai_provider_core::ProviderError::ContextWindowExceeded(
                "test context limit".into(),
            )
            .into());
        }
        let mut events: Vec<_> = self.events.iter().cloned().map(Ok).collect();
        if attempt <= self.interruptions {
            events.push(Err(kraai_provider_core::ProviderError::StreamInterrupted(
                "disconnected".into(),
            )
            .into()));
        }
        if self.fail {
            events.push(Err(eyre!("summary stream disconnected")));
        }
        Ok(Box::pin(stream::iter(events)))
    }
}

fn fixture(
    native: bool,
    mut events: Vec<ProviderStreamEvent>,
    fail: bool,
) -> (
    ContextCompaction,
    ProviderManager,
    Requests,
    std::path::PathBuf,
) {
    let root = std::env::temp_dir().join(format!("compaction-agent-{}", ulid::Ulid::generate()));
    let history = vec![
        message(
            "user",
            ConversationItem::User {
                text: "Preserve the filesystem".into(),
            },
        ),
        message(
            "call",
            ConversationItem::Assistant {
                items: vec![
                    AssistantItem::Reasoning {
                        provider_id: ProviderId::new("test"),
                        payload: serde_json::json!({"type":"reasoning","encrypted_content":"encrypted-reasoning"}),
                    },
                    AssistantItem::ScriptCall {
                        call_id: ToolCallId::new("call"),
                        name: "kraai_nushell".into(),
                        input: "inspect".into(),
                    },
                ],
            },
        ),
        message(
            "result",
            ConversationItem::ScriptResult {
                call_id: ToolCallId::new("call"),
                output: "firmware output ".repeat(20_000),
            },
        ),
    ];
    let original = assemble(
        "instructions",
        "pinned files",
        None,
        &history,
        Some(kraai_provider_core::ScriptToolDefinition {
            name: "kraai_nushell".into(),
            description: "Run script".into(),
        }),
    );
    let context = ContextCompaction {
        store: FileCompactionStore::new(&root),
        usage_store: Arc::new(kraai_persistence::FileRequestUsageStore::new(&root)),
        session_id: "session".into(),
        original,
        prefix: "instructions".into(),
        suffix: "pinned files".into(),
        history,
        previous: None,
        on_usage: None,
        usage_barrier: None,
    };
    events.push(ProviderStreamEvent::Usage(TokenUsage {
        input_tokens: 100,
        output_tokens: 10,
        ..Default::default()
    }));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut manager = ProviderManager::new();
    manager.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            requests: requests.clone(),
            native,
            events,
            fail,
            reject_large_input: false,
            interruptions: 0,
        }),
    );
    (context, manager, requests, root)
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

#[tokio::test]
async fn native_compaction_preserves_encrypted_input_and_persists_replayable_output() -> Result<()>
{
    let payload = serde_json::json!({"type":"compaction","encrypted_content":"opaque-checkpoint","id":"cmp-1"});
    let (context, providers, requests, root) = fixture(
        true,
        vec![ProviderStreamEvent::Compaction {
            payload: payload.clone(),
        }],
        false,
    );
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await?;
    {
        let requests = requests.lock().map_err(|e| eyre!("{e}"))?;
        ensure!(requests.len() == 1);
        let sent = requests.first().ok_or_else(|| eyre!("missing request"))?;
        ensure!(sent.messages == context.original.messages);
        ensure!(sent.script_tool == context.original.script_tool);
        drop(requests);
    }
    ensure!(outcome.requests.len() == 1 && outcome.requests.iter().all(|r| r.usage.is_some()));
    let saved = FileCompactionStore::new(&root)
        .get(&MessageId::new("result"))
        .await?
        .ok_or_else(|| eyre!("missing checkpoint"))?;
    ensure!(
        saved.replacement
            == vec![
                ConversationItem::User {
                    text: "Preserve the filesystem".into()
                },
                ConversationItem::Compaction {
                    provider_id: ProviderId::new("test"),
                    payload: payload.clone()
                },
            ]
    );
    ensure!(saved.compatible_with(&ProviderId::new("test"), &ModelId::new("model")));
    ensure!(!saved.compatible_with(&ProviderId::new("other"), &ModelId::new("model")));
    ensure!(!saved.compatible_with(&ProviderId::new("test"), &ModelId::new("other")));
    let replay = assemble(
        "new instructions",
        "updated files",
        Some(&saved),
        &[],
        context.original.script_tool.clone(),
    );
    ensure!(replay.messages.get(2..4) == Some(saved.replacement.as_slice()));
    ensure!(outcome.request.cacheable_messages == Some(1));
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test]
async fn fallback_uses_one_conversation_request_and_accepts_large_summary() -> Result<()> {
    let summary = "Important progress and next steps. ".repeat(3000);
    let (context, providers, requests, root) = fixture(
        false,
        vec![ProviderStreamEvent::TextDelta {
            item_id: "summary".into(),
            phase: AssistantPhase::FinalAnswer,
            delta: summary.clone(),
        }],
        false,
    );
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await?;
    {
        let requests = requests.lock().map_err(|e| eyre!("{e}"))?;
        ensure!(requests.len() == 1);
        let sent = requests.first().ok_or_else(|| eyre!("missing request"))?;
        ensure!(
            sent.messages.get(..context.original.messages.len())
                == Some(context.original.messages.as_slice())
        );
        ensure!(
            matches!(sent.messages.last(), Some(ConversationItem::User { text }) if text.contains("CONTEXT CHECKPOINT"))
        );
        drop(requests);
    }
    ensure!(
        outcome
            .request
            .messages
            .iter()
            .any(|item| item.display_text().contains(&summary))
    );
    ensure!(
        !outcome
            .request
            .messages
            .iter()
            .any(|item| matches!(item, ConversationItem::ScriptResult { .. }))
    );
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test]
async fn failed_or_malformed_native_compaction_never_replaces_history_or_uses_text_fallback()
-> Result<()> {
    let payload = serde_json::json!({"type":"compaction","encrypted_content":"opaque"});
    for (events, fail) in [
        (vec![], false),
        (
            vec![
                ProviderStreamEvent::Compaction {
                    payload: payload.clone()
                };
                2
            ],
            false,
        ),
        (vec![ProviderStreamEvent::Compaction { payload }], true),
    ] {
        let (context, providers, requests, root) = fixture(true, events, fail);
        ensure!(
            context
                .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
                .await
                .is_err()
        );
        ensure!(requests.lock().map_err(|e| eyre!("{e}"))?.len() == 1);
        ensure!(
            context
                .store
                .get(&MessageId::new("result"))
                .await?
                .is_none()
        );
        ensure!(context.usage_store.load("session").await?.len() == 1);
        tokio::fs::remove_dir_all(root).await?;
    }
    Ok(())
}

#[test]
fn user_retention_prioritizes_recent_requests_and_preserves_unicode_boundaries() {
    let messages = vec![
        ConversationItem::User {
            text: "older".into(),
        },
        ConversationItem::User {
            text: "😀中hello".into(),
        },
    ];
    assert_eq!(
        retained_users(&messages, 2),
        vec![ConversationItem::User {
            text: "😀中h".into()
        }]
    );
}

#[tokio::test]
async fn fallback_context_overflow_trims_complete_exchanges_and_keeps_original_user() -> Result<()>
{
    let (context, mut providers, requests, root) = fixture(false, vec![], false);
    providers.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            requests: requests.clone(),
            native: false,
            fail: false,
            reject_large_input: true,
            interruptions: 0,
            events: vec![ProviderStreamEvent::TextDelta {
                item_id: "summary".into(),
                phase: AssistantPhase::FinalAnswer,
                delta: "continue investigation".into(),
            }],
        }),
    );
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await?;
    {
        let requests = requests.lock().map_err(|e| eyre!("{e}"))?;
        ensure!(requests.len() == 3);
        let last = requests.last().ok_or_else(|| eyre!("missing request"))?;
        ensure!(!last.messages.iter().any(|item| matches!(
            item,
            ConversationItem::ScriptResult { .. } | ConversationItem::Assistant { .. }
        )));
        drop(requests);
    }
    ensure!(outcome.request.messages.iter().any(
        |item| matches!(item, ConversationItem::User { text } if text == "Preserve the filesystem")
    ));
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn native_stream_retries_are_bounded_and_each_attempt_is_accounted() -> Result<()> {
    let (context, mut providers, requests, root) = fixture(true, vec![], false);
    providers.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            requests: requests.clone(),
            native: true,
            fail: false,
            reject_large_input: false,
            interruptions: usize::MAX,
            events: vec![ProviderStreamEvent::Usage(TokenUsage {
                input_tokens: 10,
                ..Default::default()
            })],
        }),
    );
    ensure!(
        context
            .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
            .await
            .is_err()
    );
    ensure!(requests.lock().map_err(|e| eyre!("{e}"))?.len() == 3);
    let usage = context.usage_store.load("session").await?;
    ensure!(usage.len() == 3);
    ensure!(
        context
            .store
            .get(&MessageId::new("result"))
            .await?
            .is_none()
    );
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test]
async fn empty_fallback_response_is_installed_without_rejection() -> Result<()> {
    let (context, providers, _, root) = fixture(false, vec![], false);
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await?;
    ensure!(
        outcome
            .request
            .messages
            .iter()
            .any(|item| item.display_text().contains("(no summary available)"))
    );
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn native_compaction_recovers_after_interruption_without_duplicate_checkpoint_items()
-> Result<()> {
    let (context, mut providers, requests, root) = fixture(true, vec![], false);
    let payload = serde_json::json!({"type":"compaction","encrypted_content":"checkpoint"});
    providers.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            requests: requests.clone(),
            native: true,
            fail: false,
            reject_large_input: false,
            interruptions: 1,
            events: vec![
                ProviderStreamEvent::Compaction {
                    payload: payload.clone(),
                },
                ProviderStreamEvent::Usage(TokenUsage {
                    input_tokens: 10,
                    ..Default::default()
                }),
            ],
        }),
    );
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await?;
    ensure!(outcome.requests.len() == 2);
    {
        let requests = requests.lock().map_err(|e| eyre!("{e}"))?;
        ensure!(requests.len() == 2);
        ensure!(
            requests
                .iter()
                .all(|request| request.messages == context.original.messages)
        );
    }
    let saved = context
        .store
        .get(&MessageId::new("result"))
        .await?
        .ok_or_else(|| eyre!("missing checkpoint"))?;
    let checkpoints: Vec<_> = saved
        .replacement
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Compaction { payload, .. } => Some(payload),
            _ => None,
        })
        .collect();
    ensure!(checkpoints == vec![&payload]);
    ensure!(context.usage_store.load("session").await?.len() == 2);
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[test]
fn user_retention_stops_when_no_character_fits_the_remaining_budget() {
    let messages = vec![
        ConversationItem::User {
            text: "older".into(),
        },
        ConversationItem::User {
            text: "😀".into()
        },
        ConversationItem::User { text: "abc".into() },
    ];
    assert_eq!(
        retained_users(&messages, 1),
        vec![ConversationItem::User { text: "abc".into() }]
    );
}

#[test]
fn empty_user_messages_do_not_discard_older_requests() {
    let messages = vec![
        ConversationItem::User {
            text: "older".into(),
        },
        ConversationItem::User {
            text: String::new(),
        },
        ConversationItem::User {
            text: "newer".into(),
        },
    ];
    assert_eq!(retained_users(&messages, 10), messages);
}
