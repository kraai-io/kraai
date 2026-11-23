use super::*;
use color_eyre::eyre::ensure;
use futures::stream::{self, BoxStream};
use kraai_persistence::FileContextSnapshot;
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderRequestContext, ProviderStreamEvent,
};
use kraai_types::{
    AssistantItem, AssistantPhase, MessageId, MessageStatus, ScriptCall, TokenUsage, ToolCallId,
};
use std::sync::Mutex;

type Requests = Arc<Mutex<Vec<ProviderRequest>>>;

struct Summarizer {
    requests: Requests,
    native: bool,
    events: Vec<ProviderStreamEvent>,
    fail: bool,
    large_input: Option<Overflow>,
    interruptions: usize,
    calls_tool: bool,
}

#[derive(Clone, Copy)]
enum Overflow {
    Rejected,
    Truncated,
}

#[async_trait::async_trait]
impl Provider for Summarizer {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("test")
    }
    async fn list_models(&self) -> Vec<Model> {
        Vec::new()
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
        context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        ensure!(self.native);
        kraai_provider_core::ResolvedImages::resolve(&request.messages, context).await?;
        self.respond(request)
    }
    async fn generate_reply_stream(
        &self,
        _: &ModelId,
        request: ProviderRequest,
        context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        ensure!(
            !self.native,
            "Native compaction must not use text summarization"
        );
        kraai_provider_core::ResolvedImages::resolve(&request.messages, context).await?;
        self.respond(request)
    }
}

impl Summarizer {
    fn respond(
        &self,
        request: ProviderRequest,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let too_large = request.messages.iter().any(|item| matches!(item, ConversationItem::ScriptResult { output, .. } if output.display_text().len() > 1000));
        let tool_call = self.calls_tool && request.script_tool.is_some();
        let attempt = {
            let mut requests = self.requests.lock().map_err(|e| eyre!("{e}"))?;
            requests.push(request);
            requests.len()
        };
        match self.large_input.filter(|_| too_large) {
            Some(Overflow::Rejected) => {
                return Err(kraai_provider_core::ProviderError::ContextWindowExceeded(
                    "test context limit".into(),
                )
                .into());
            }
            Some(Overflow::Truncated) => {
                return Ok(Box::pin(stream::iter([
                    Ok(ProviderStreamEvent::TextDelta {
                        item_id: "summary".into(),
                        phase: AssistantPhase::FinalAnswer,
                        delta: "cut off".into(),
                    }),
                    Ok(ProviderStreamEvent::Finished {
                        stop_reason: kraai_provider_core::StopReason::MaxOutputTokens,
                    }),
                ])));
            }
            None => {}
        }
        if tool_call {
            return Ok(Box::pin(stream::iter([Ok(
                ProviderStreamEvent::ScriptCall(ScriptCall {
                    call_id: ToolCallId::new("unexpected"),
                    name: "kraai_nushell".into(),
                    input: "ls".into(),
                    arguments_error: None,
                    rewrite: None,
                }),
            )])));
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

fn files(text: &str) -> RefreshedContextState {
    RefreshedContextState {
        snapshots: vec![FileContextSnapshot {
            path: "file.rs".into(),
            opened_event: "open".into(),
            anchor: MessageId::new("result"),
            text: text.into(),
        }],
        ..Default::default()
    }
}

struct Prepared {
    compaction: ContextCompaction,
    original: ProviderRequest,
}

impl Prepared {
    fn with_image_resolver(
        mut self,
        resolver: Arc<dyn kraai_provider_core::ImageResolver>,
    ) -> Self {
        self.compaction = self.compaction.with_image_resolver(resolver);
        self
    }

    async fn run(
        &self,
        providers: &ProviderManager,
        provider_id: &ProviderId,
        model_id: &ModelId,
    ) -> Result<CompactionOutcome> {
        self.compaction
            .run(&self.original, providers, provider_id, model_id)
            .await
    }
}

impl std::ops::Deref for Prepared {
    type Target = ContextCompaction;

    fn deref(&self) -> &ContextCompaction {
        &self.compaction
    }
}

impl std::ops::DerefMut for Prepared {
    fn deref_mut(&mut self) -> &mut ContextCompaction {
        &mut self.compaction
    }
}

fn fixture(
    native: bool,
    mut events: Vec<ProviderStreamEvent>,
    fail: bool,
) -> (Prepared, ProviderManager, Requests, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("compaction-agent-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&root).expect("create compaction data directory");
    let history = vec![
        message(
            "user",
            ConversationItem::User {
                content: "Preserve the filesystem".into(),
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
                    AssistantItem::ScriptCall(ScriptCall {
                        call_id: ToolCallId::new("call"),
                        name: "kraai_nushell".into(),
                        input: "inspect".into(),
                        arguments_error: None,
                        rewrite: None,
                    }),
                ],
            },
        ),
        message(
            "result",
            ConversationItem::ScriptResult {
                outcome: kraai_execution_types::ScriptExecutionOutcome {
                    status: kraai_execution_types::ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                },
                call_id: ToolCallId::new("call"),
                output: "firmware output ".repeat(20_000).into(),
            },
        ),
    ];
    let covered_through = history.last().map(|message| message.id.clone());
    let original = assemble(
        "instructions",
        &files("pinned files"),
        None,
        history,
        Some(kraai_provider_core::ScriptToolDefinition::nushell()),
        &Default::default(),
    );
    let context = Prepared {
        compaction: ContextCompaction {
            store: SqliteCompactionStore::new(&root),
            usage_store: Arc::new(kraai_persistence::SqliteRequestUsageStore::new(&root)),
            session_id: SessionId::new("session"),
            prefix: "instructions".into(),
            context: files("pinned files"),
            covered_through,
            previous_boundary: None,
            on_usage: None,
            usage_barrier: None,
            image_resolver: None,
        },
        original,
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
            large_input: None,
            interruptions: 0,
            calls_tool: false,
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
async fn native_compaction_preserves_encrypted_input_and_persists_replayable_output() {
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
        .await
        .unwrap();
    {
        let requests = requests.lock().unwrap();
        assert!(requests.len() == 1);
        let sent = requests.first().expect("missing request");
        assert!(sent.messages == context.original.messages);
        assert!(sent.script_tool == context.original.script_tool);
        drop(requests);
    }
    assert!(outcome.requests.len() == 1 && outcome.requests.iter().all(|r| r.usage.is_some()));
    let saved = SqliteCompactionStore::new(&root)
        .get(&MessageId::new("result"))
        .await
        .unwrap()
        .expect("missing checkpoint");
    assert!(
        saved.replacement
            == vec![
                ConversationItem::User {
                    content: "Preserve the filesystem".into()
                },
                ConversationItem::Compaction {
                    provider_id: ProviderId::new("test"),
                    payload: payload.clone()
                },
            ]
    );
    assert!(saved.compatible_with(&ProviderId::new("test"), &ModelId::new("model")));
    assert!(!saved.compatible_with(&ProviderId::new("other"), &ModelId::new("model")));
    assert!(!saved.compatible_with(&ProviderId::new("test"), &ModelId::new("other")));
    let replay = assemble(
        "new instructions",
        &files("updated files"),
        Some(&saved),
        Vec::new(),
        context.original.script_tool.clone(),
        &Default::default(),
    );
    assert!(replay.messages.get(1..3) == Some(saved.replacement.as_slice()));
    assert!(outcome.request.cacheable_messages.is_none());
    assert!(
        matches!(outcome.request.messages.last(), Some(ConversationItem::FileContext { text }) if text == "pinned files")
    );
    assert!(
        outcome
            .request
            .messages
            .iter()
            .filter(|item| matches!(item, ConversationItem::FileContext { .. }))
            .count()
            == 1
    );
    assert!(
        !saved
            .replacement
            .iter()
            .any(|item| matches!(item, ConversationItem::FileContext { .. }))
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn fallback_uses_one_conversation_request_and_accepts_large_summary() {
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
        .await
        .unwrap();
    {
        let requests = requests.lock().unwrap();
        assert!(requests.len() == 1);
        let sent = requests.first().expect("missing request");
        let expected = &context.original.messages;
        assert!(sent.messages.get(..expected.len()) == Some(expected.as_slice()));
        assert!(sent.messages.len() == expected.len() + 1);
        assert!(sent.script_tool == context.original.script_tool);
        assert!(
            matches!(sent.messages.last(), Some(ConversationItem::User { content: text }) if text.display_text().contains("CONTEXT CHECKPOINT"))
        );
        drop(requests);
    }
    assert!(
        outcome
            .request
            .messages
            .iter()
            .any(|item| item.display_text().contains(&summary))
    );
    assert!(
        !outcome
            .request
            .messages
            .iter()
            .any(|item| matches!(item, ConversationItem::ScriptResult { .. }))
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn failed_or_malformed_native_compaction_never_replaces_history_or_uses_text_fallback() {
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
        assert!(
            context
                .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
                .await
                .is_err()
        );
        assert!(requests.lock().unwrap().len() == 1);
        assert!(
            context
                .store
                .get(&MessageId::new("result"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            context
                .usage_store
                .load(&SessionId::new("session"))
                .await
                .unwrap()
                .len()
                == 1
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}

#[test]
fn user_retention_prioritizes_recent_requests_and_preserves_unicode_boundaries() {
    let messages = vec![
        ConversationItem::User {
            content: "older".into(),
        },
        ConversationItem::User {
            content: "😀中hello".into(),
        },
    ];
    assert_eq!(
        retained_users(&messages, 2),
        vec![ConversationItem::User {
            content: "😀中h".into()
        }]
    );
}

#[tokio::test]
async fn fallback_context_overflow_trims_complete_exchanges_and_keeps_original_user() {
    for overflow in [Overflow::Rejected, Overflow::Truncated] {
        trims_complete_exchanges_after(overflow).await;
    }
}

async fn trims_complete_exchanges_after(overflow: Overflow) {
    let (context, mut providers, requests, root) = fixture(false, vec![], false);
    providers.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            requests: requests.clone(),
            native: false,
            fail: false,
            large_input: Some(overflow),
            interruptions: 0,
            calls_tool: false,
            events: vec![ProviderStreamEvent::TextDelta {
                item_id: "summary".into(),
                phase: AssistantPhase::FinalAnswer,
                delta: "continue investigation".into(),
            }],
        }),
    );
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await
        .unwrap();
    {
        let requests = requests.lock().unwrap();
        assert!(requests.len() == 3);
        let last = requests.last().expect("missing request");
        assert!(!last.messages.iter().any(|item| matches!(
            item,
            ConversationItem::ScriptResult { .. } | ConversationItem::Assistant { .. }
        )));
        drop(requests);
    }
    assert!(outcome.request.messages.iter().any(
        |item| matches!(item, ConversationItem::User { content: text } if text.display_text() == "Preserve the filesystem")
    ));
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn native_stream_retries_are_bounded_and_each_attempt_is_accounted() {
    let (context, mut providers, requests, root) = fixture(true, vec![], false);
    providers.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            requests: requests.clone(),
            native: true,
            fail: false,
            large_input: None,
            interruptions: usize::MAX,
            calls_tool: false,
            events: vec![ProviderStreamEvent::Usage(TokenUsage {
                input_tokens: 10,
                ..Default::default()
            })],
        }),
    );
    assert!(
        context
            .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
            .await
            .is_err()
    );
    assert!(requests.lock().unwrap().len() == 3);
    let usage = context
        .usage_store
        .load(&SessionId::new("session"))
        .await
        .unwrap();
    assert!(usage.len() == 3);
    assert!(
        context
            .store
            .get(&MessageId::new("result"))
            .await
            .unwrap()
            .is_none()
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn invalid_fallback_response_preserves_existing_context() {
    for events in [
        vec![],
        vec![ProviderStreamEvent::ScriptCall(ScriptCall {
            call_id: ToolCallId::new("unexpected"),
            name: "kraai_nushell".into(),
            input: "# timeout=1sec\nls".into(),
            arguments_error: None,
            rewrite: None,
        })],
    ] {
        let (context, providers, _, root) = fixture(false, events, false);
        assert!(
            context
                .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
                .await
                .is_err()
        );
        assert!(
            context
                .store
                .get(&MessageId::new("result"))
                .await
                .unwrap()
                .is_none()
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}

#[tokio::test]
async fn fallback_retries_without_the_tool_after_an_unexpected_tool_call() {
    let (context, mut providers, requests, root) = fixture(false, vec![], false);
    providers.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            requests: requests.clone(),
            native: false,
            fail: false,
            large_input: None,
            interruptions: 0,
            calls_tool: true,
            events: vec![ProviderStreamEvent::TextDelta {
                item_id: "summary".into(),
                phase: AssistantPhase::FinalAnswer,
                delta: "continue investigation".into(),
            }],
        }),
    );
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await
        .unwrap();
    {
        let requests = requests.lock().unwrap();
        assert!(requests.len() == 2);
        assert!(requests.first().is_some_and(|r| r.script_tool.is_some()));
        assert!(requests.last().is_some_and(|r| {
            r.script_tool.is_none()
                && requests
                    .first()
                    .is_some_and(|first| first.messages == r.messages)
        }));
        drop(requests);
    }
    assert!(
        outcome
            .request
            .messages
            .iter()
            .any(|item| item.display_text().contains("continue investigation"))
    );
    assert!(outcome.request.script_tool == context.original.script_tool);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn native_compaction_recovers_after_interruption_without_duplicate_checkpoint_items() {
    let (context, mut providers, requests, root) = fixture(true, vec![], false);
    let payload = serde_json::json!({"type":"compaction","encrypted_content":"checkpoint"});
    providers.register_provider(
        ProviderId::new("test"),
        Box::new(Summarizer {
            requests: requests.clone(),
            native: true,
            fail: false,
            large_input: None,
            interruptions: 1,
            calls_tool: false,
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
        .await
        .unwrap();
    assert!(outcome.requests.len() == 2);
    {
        let requests = requests.lock().unwrap().clone();
        assert!(requests.len() == 2);
        assert!(
            requests
                .iter()
                .all(|request| request.messages == context.original.messages)
        );
    }
    let saved = context
        .store
        .get(&MessageId::new("result"))
        .await
        .unwrap()
        .expect("missing checkpoint");
    let checkpoints: Vec<_> = saved
        .replacement
        .iter()
        .filter_map(|item| {
            if let ConversationItem::Compaction { payload, .. } = item {
                Some(payload)
            } else {
                None
            }
        })
        .collect();
    assert!(checkpoints == vec![&payload]);
    assert!(
        context
            .usage_store
            .load(&SessionId::new("session"))
            .await
            .unwrap()
            .len()
            == 2
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[test]
fn user_retention_stops_when_no_character_fits_the_remaining_budget() {
    let messages = vec![
        ConversationItem::User {
            content: "older".into(),
        },
        ConversationItem::User {
            content: "😀".into(),
        },
        ConversationItem::User {
            content: "abc".into(),
        },
    ];
    assert_eq!(
        retained_users(&messages, 1),
        vec![ConversationItem::User {
            content: "abc".into()
        }]
    );
}

#[test]
fn empty_user_messages_do_not_discard_older_requests() {
    let messages = vec![
        ConversationItem::User {
            content: "older".into(),
        },
        ConversationItem::User {
            content: String::new().into(),
        },
        ConversationItem::User {
            content: "newer".into(),
        },
    ];
    assert_eq!(retained_users(&messages, 10), messages);
}

#[test]
fn image_request_budget_keeps_recent_images_and_reopenable_references() {
    let image = |index: usize| kraai_types::ContentPart::Image {
        image: kraai_execution_types::ImageAttachment {
            id: format!("{index:064x}"),
            mime_type: "image/png".into(),
            width: 1,
            height: 1,
            byte_length: 100,
        },
    };
    let mut request = ProviderRequest {
        options: Default::default(),
        messages: (0..40)
            .map(|index| ConversationItem::User {
                content: kraai_types::MessageContent(vec![image(index)]),
            })
            .collect(),
        script_tool: None,
        cacheable_messages: None,
    };
    limit_request_images(&mut request);
    let count = request
        .messages
        .iter()
        .filter_map(|message| {
            if let ConversationItem::User { content } = message {
                Some(content.images().count())
            } else {
                None
            }
        })
        .sum::<usize>();
    assert_eq!(count, kraai_execution_types::image::MAX_REQUEST_IMAGES);
    assert!(request.messages.first().is_some_and(|message| {
        message
            .display_text()
            .contains("kraai-view-image --attachment")
    }));
    assert!(
        matches!(request.messages.last(), Some(ConversationItem::User { content }) if content.has_images())
    );
}

#[tokio::test]
async fn compaction_preserves_options_for_summary_and_following_generation() {
    for native in [false, true] {
        let events = if native {
            vec![ProviderStreamEvent::Compaction {
                payload: serde_json::json!({"type":"compaction","encrypted_content":"opaque"}),
            }]
        } else {
            vec![ProviderStreamEvent::TextDelta {
                item_id: "summary".into(),
                phase: AssistantPhase::FinalAnswer,
                delta: "Retained context".into(),
            }]
        };
        let (mut context, providers, requests, root) = fixture(native, events, false);
        let options = kraai_types::ModelOptionValues::from([
            (
                "effort".into(),
                kraai_types::ModelOptionValue::Choice("high".into()),
            ),
            (
                "fast_mode".into(),
                kraai_types::ModelOptionValue::Boolean(true),
            ),
        ]);
        context.original.options = options.clone();
        let outcome = context
            .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
            .await
            .unwrap();
        assert!(outcome.request.options == options);
        let requests = requests.lock().unwrap();
        assert!(!requests.is_empty());
        assert!(requests.iter().all(|request| request.options == options));
        drop(requests);
        std::fs::remove_dir_all(root).unwrap();
    }
}

struct TestImageResolver;

#[async_trait::async_trait]
impl kraai_provider_core::ImageResolver for TestImageResolver {
    async fn resolve(&self, _: &kraai_execution_types::ImageAttachment) -> Result<Vec<u8>> {
        Ok(vec![1, 2, 3])
    }
}

#[tokio::test]
async fn compaction_reads_images_but_retains_only_reopenable_references() {
    let image = kraai_execution_types::ImageAttachment {
        id: "a".repeat(64),
        mime_type: "image/png".into(),
        width: 2,
        height: 3,
        byte_length: 3,
    };
    for native in [false, true] {
        let events = if native {
            vec![ProviderStreamEvent::Compaction {
                payload: serde_json::json!({"type":"compaction","encrypted_content":"opaque"}),
            }]
        } else {
            vec![ProviderStreamEvent::TextDelta {
                item_id: "summary".into(),
                phase: AssistantPhase::FinalAnswer,
                delta: "Screenshot inspected".into(),
            }]
        };
        let (mut context, providers, requests, root) = fixture(native, events, false);
        let user = ConversationItem::User {
            content: kraai_types::MessageContent(vec![
                kraai_types::ContentPart::Text {
                    text: "Inspect the screenshot".into(),
                },
                kraai_types::ContentPart::Image {
                    image: image.clone(),
                },
            ]),
        };
        context.original.messages.insert(1, user.clone());
        let outcome = context
            .with_image_resolver(Arc::new(TestImageResolver))
            .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
            .await
            .unwrap();
        {
            let requests = requests.lock().unwrap().clone();
            assert!(
                requests
                    .first()
                    .is_some_and(|request| request.messages.contains(&user))
            );
        }
        let saved = SqliteCompactionStore::new(&root)
            .get(&MessageId::new("result"))
            .await
            .unwrap()
            .expect("missing checkpoint");
        for messages in [&outcome.request.messages, &saved.replacement] {
            assert!(messages.iter().all(|message| !matches!(message, ConversationItem::User { content } if content.has_images())));
            assert!(
                messages
                    .iter()
                    .any(|message| message.display_text().contains(&image.id))
            );
            assert!(
                messages
                    .iter()
                    .any(|message| message.display_text().contains("Inspect the screenshot"))
            );
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}

#[tokio::test]
async fn file_removal_notifications_survive_compaction_without_becoming_retained_users() {
    let (mut context, providers, _, root) = fixture(
        true,
        vec![ProviderStreamEvent::Compaction {
            payload: serde_json::json!({"type":"compaction", "encrypted_content":"opaque"}),
        }],
        false,
    );
    context.context.notifications =
        vec!["removed.txt was automatically unpinned because it no longer exists.".into()];
    crate::context_state::append_notifications(
        &mut context.original,
        &context.compaction.context.notifications,
    );
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await
        .unwrap();
    assert!(
        outcome
            .request
            .messages
            .last()
            .is_some_and(|item| item.display_text().contains("removed.txt"))
    );
    let checkpoint = context
        .store
        .get(&MessageId::new("result"))
        .await
        .unwrap()
        .expect("missing checkpoint");
    assert!(
        !checkpoint
            .replacement
            .iter()
            .any(|item| item.display_text().contains("removed.txt"))
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}

mod chat_completions;

mod assembly;
