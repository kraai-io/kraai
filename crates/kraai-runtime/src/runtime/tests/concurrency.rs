use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use color_eyre::eyre::Result;
use futures::{StreamExt, poll};
use kraai_persistence::{MessageStore, SqliteMessageStore};
use kraai_test_support::hang_guard;
use kraai_types::{Message, MessageId};
use tokio_util::sync::CancellationToken;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::{Event, SessionActivity};

struct HandoffProvider {
    requests: tokio::sync::mpsc::UnboundedSender<kraai_provider_core::ProviderRequest>,
    first_request: AtomicBool,
    hold_after_call: bool,
    text_before_call: Option<&'static str>,
}

#[async_trait::async_trait]
impl kraai_provider_core::Provider for HandoffProvider {
    fn get_provider_id(&self) -> kraai_types::ProviderId {
        kraai_types::ProviderId::new("mock")
    }

    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![super::harness::mock_model()]
    }

    async fn register_model(&mut self, _model: kraai_provider_core::ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &kraai_types::ModelId,
        request: kraai_provider_core::ProviderRequest,
        _context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<futures::stream::BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>>
    {
        self.requests.send(request)?;
        if self.first_request.swap(false, Ordering::SeqCst) {
            let mut events = Vec::new();
            if let Some(text) = self.text_before_call {
                events.push(Ok(kraai_provider_core::ProviderStreamEvent::TextDelta {
                    item_id: "partial-text".into(),
                    phase: kraai_types::AssistantPhase::Commentary,
                    delta: text.into(),
                }));
            }
            events.push(Ok(kraai_provider_core::ProviderStreamEvent::ScriptCall {
                call_id: kraai_types::ToolCallId::new("handoff-call"),
                name: String::from("kraai_nushell"),
                input: String::from(
                    "# timeout=30sec permissions=workspace-write\n'changed' | save result.txt",
                ),
                arguments_error: None,
            }));
            let events = futures::stream::iter(events);
            if self.hold_after_call {
                Ok(Box::pin(events.chain(futures::stream::pending())))
            } else {
                Ok(Box::pin(events))
            }
        } else {
            Ok(Box::pin(futures::stream::pending()))
        }
    }
}

#[tokio::test]
async fn cancellation_after_script_boundary_keeps_the_next_provider_history_valid() -> Result<()> {
    assert_cancellation_history(None).await
}

#[tokio::test]
async fn cancelled_script_result_save_failure_can_be_retried() -> Result<()> {
    assert_cancellation_history(Some(false)).await
}

#[tokio::test]
async fn cancelled_script_result_retry_links_an_already_saved_result_once() -> Result<()> {
    assert_cancellation_history(Some(true)).await
}

async fn assert_cancellation_history(fail_after_save: Option<bool>) -> Result<()> {
    let data_dir =
        std::env::temp_dir().join(format!("kraai-cancelled-script-{}", ulid::Ulid::generate()));
    tokio::fs::create_dir_all(&data_dir).await?;
    let store = Arc::new(PausedMessageStore {
        inner: SqliteMessageStore::new(&data_dir),
        pause: AtomicBool::new(false),
        pause_completed_script: AtomicBool::new(false),
        fail_script_result_save: AtomicBool::new(fail_after_save.is_some()),
        persist_failed_script_result: fail_after_save.unwrap_or(false),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let (requests, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut providers = kraai_provider_core::ProviderManager::new();
    providers.register_provider(
        kraai_types::ProviderId::new("mock"),
        Box::new(HandoffProvider {
            requests,
            first_request: AtomicBool::new(true),
            hold_after_call: true,
            text_before_call: Some("I am partway through."),
        }),
    );
    let harness = RuntimeTestHarness::new_with_message_store(providers, Some(store.clone()))
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "change it".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    hang_guard(received.recv())
        .await?
        .expect("first provider request");
    harness
        .events
        .wait_for("script call before usage drain completes", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ItemStarted { session_id: id, item: kraai_types::AssistantItem::ScriptCall { .. }, .. } if id == &session_id)
            })
        })
        .await;
    if fail_after_save.is_some() {
        let error = harness
            .handle
            .cancel_stream(session_id.clone())
            .await
            .expect_err("result persistence must fail once");
        assert!(
            error
                .message
                .contains("injected cancelled result save failure")
        );
        let snapshot = harness
            .handle
            .get_session_snapshot(session_id.clone())
            .await?;
        assert!(snapshot.session.is_running);
        assert!(snapshot.session.is_streaming);
    }
    assert!(harness.handle.cancel_stream(session_id.clone()).await?);
    assert!(!harness.handle.cancel_stream(session_id.clone()).await?);
    assert!(harness.runtime.execution_store.list_all().await?.is_empty());
    assert!(
        received.try_recv().is_err(),
        "cancellation started an automatic continuation"
    );
    harness
        .handle
        .send_message(
            session_id,
            "skip that, do something else".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    let request = hang_guard(received.recv())
        .await?
        .expect("provider request after cancellation");
    harness.shutdown().await;
    let has_call = request.messages.iter().any(|item| {
        matches!(item, kraai_types::ConversationItem::Assistant { items }
            if items.iter().any(|item| matches!(item, kraai_types::AssistantItem::ScriptCall { call_id, .. }
                if call_id.as_str() == "handoff-call")))
    });
    assert!(has_call, "cancellation discarded the script call");
    let results = request
        .messages
        .iter()
        .filter_map(|item| {
            if let kraai_types::ConversationItem::ScriptResult {
                call_id, output, ..
            } = item
                && call_id.as_str() == "handoff-call"
            {
                Some(output)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        results.len(),
        1,
        "cancelled script needs exactly one matching result"
    );
    assert!(
        results
            .first()
            .expect("cancelled script result")
            .display_text()
            .contains("status=\"cancelled\"")
    );
    assert!(request.messages.iter().any(|item| {
        matches!(item, kraai_types::ConversationItem::Assistant { items }
            if items.iter().any(|item| matches!(item, kraai_types::AssistantItem::Text { phase, text }
                if *phase == kraai_types::AssistantPhase::Commentary && text == "I am partway through.")))
    }));
    let mut result_count = 0;
    let mut on_disk: Vec<_> = store.list_ids().await?.into_iter().collect();
    on_disk.sort();
    for id in on_disk {
        if let Some(message) = store.get(&id).await?
            && matches!(
                message.content,
                kraai_types::ConversationItem::ScriptResult { .. }
            )
        {
            result_count += 1;
        }
    }
    assert_eq!(result_count, 1, "retry left duplicate durable results");
    tokio::fs::remove_dir_all(data_dir).await?;
    Ok(())
}

#[tokio::test]
async fn held_preparation_queues_without_blocking_another_session() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain("other session complete")],
        vec![ScriptedChunk::plain("queued message complete")],
    ])
    .await
    .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let other_session = create_session_with_profile(&harness.handle, "test-profile").await?;
    super::harness::hold_preparation(&harness.runtime, &session_id).await;
    assert!(matches!(
        harness
            .handle
            .send_message(
                session_id.clone(),
                "queued".into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
            )
            .await?,
        crate::SubmitMessageOutcome::Queued { position: 1 }
    ));
    assert!(matches!(
        hang_guard(harness.handle.send_message(
            other_session,
            "independent".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        ),)
        .await??,
        crate::SubmitMessageOutcome::Started { .. }
    ));
    super::harness::release_preparation(&harness.runtime, &session_id).await;
    harness
        .runtime
        .with_session_actor(&session_id, |actor| {
            Box::pin(async move { actor.drain().await })
        })
        .await;
    harness
        .events
        .wait_for("deferred message started", |events| {
            events.iter().any(|event| {
                matches!(event, Event::StreamStart { session_id: id, .. } if id == &session_id)
            })
        })
        .await;
    assert!(
        super::harness::take_queue(&harness.runtime, &session_id)
            .await
            .is_empty()
    );
    harness.shutdown().await;
    Ok(())
}

fn idle_provider() -> kraai_provider_core::ProviderManager {
    let (requests, _received) = tokio::sync::mpsc::unbounded_channel();
    let mut providers = kraai_provider_core::ProviderManager::new();
    providers.register_provider(
        kraai_types::ProviderId::new("mock"),
        Box::new(HandoffProvider {
            requests,
            first_request: AtomicBool::new(false),
            hold_after_call: false,
            text_before_call: None,
        }),
    );
    providers
}

#[tokio::test]
async fn session_stays_running_between_streams_until_turn_finishes() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let idle_session = create_session_with_profile(&harness.handle, "test-profile").await?;
    let request = harness
        .runtime
        .agent_manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
            Default::default(),
        )
        .await?;

    for is_streaming in [true, false] {
        let sessions = harness.handle.list_sessions().await?;
        let session = sessions
            .iter()
            .find(|session| session.id == session_id)
            .unwrap();
        assert!(session.is_running);
        assert_eq!(session.is_streaming, is_streaming);
        assert!(
            !sessions
                .iter()
                .find(|session| session.id == idle_session)
                .unwrap()
                .is_running
        );
        let snapshot = harness
            .handle
            .get_session_snapshot(session_id.clone())
            .await?;
        assert!(snapshot.session.is_running);
        assert_eq!(snapshot.session.is_streaming, is_streaming);
        if is_streaming {
            harness
                .runtime
                .agent_manager
                .complete_message(&request.message_id)
                .await?;
        }
    }

    harness.runtime.agent_manager.clear_active_turn(&session_id);
    let sessions = harness.handle.list_sessions().await?;
    assert!(sessions.iter().all(|session| !session.is_running));
    let snapshot = harness
        .handle
        .get_session_snapshot(session_id.clone())
        .await?;
    assert!(!snapshot.session.is_running);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn stream_start_events_precede_a_snapshot_taken_right_after_the_stream_starts() -> Result<()>
{
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut request = harness
        .runtime
        .agent_manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    request.context_notifications = vec!["missing file automatically unpinned".into()];
    let message_id = request.message_id.to_string();
    let mut events = harness.runtime.event_tx.subscribe();
    let (snapshot,) = harness
        .runtime
        .with_session_actor(&session_id, move |actor| {
            Box::pin(async move {
                actor.start_test_stream(idle_provider(), request);
                (actor.request_snapshot().await,)
            })
        })
        .await;

    let timing = events.try_recv()?;
    let Event::TurnTimingChanged { timer, .. } = timing.event else {
        panic!("expected runtime turn timing");
    };
    let start = events.try_recv()?;
    assert!(
        matches!(start.event, Event::StreamStart { message_id: actual, .. } if actual == message_id)
    );
    let context = events.try_recv()?;
    assert!(
        matches!(context.event, Event::ContextStateChanged { notifications, .. } if notifications.len() == 1)
    );
    let snapshot = hang_guard(snapshot).await???;
    assert!(snapshot.event_sequence >= start.sequence);
    assert_eq!(snapshot.activity, SessionActivity::Streaming);
    assert_eq!(snapshot.turn_timer, timer);
    assert!(timer.elapsed(std::time::Instant::now()).is_some());
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancellation_finishes_after_a_snapshot_and_stays_active_until_finalized() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let runtime = harness.runtime.clone();
    let (generation, cancellation) = runtime
        .with_session_actor(&session_id, |actor| {
            Box::pin(async move { actor.begin_test_script() })
        })
        .await;

    let handle = harness.handle.clone();
    let cancel_session = session_id.clone();
    let cancel = tokio::spawn(async move { handle.cancel_stream(cancel_session).await });
    hang_guard(cancellation.cancelled()).await?;
    let snapshot = harness
        .handle
        .get_session_snapshot(session_id.clone())
        .await?;
    assert_eq!(snapshot.activity, SessionActivity::ExecutingScript);
    assert!(!cancel.is_finished());

    runtime.sessions.send(
        &runtime,
        &session_id,
        crate::runtime::session::SessionMessage::ScriptEnded {
            generation,
            result: Err(color_eyre::eyre::eyre!("script cancelled")),
        },
    );
    assert!(hang_guard(cancel).await???);
    let snapshot = harness
        .handle
        .get_session_snapshot(session_id.clone())
        .await?;
    assert_eq!(snapshot.activity, SessionActivity::Idle);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_synchronous_stream_poll_to_return() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let entered = CancellationToken::new();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task_entered = entered.clone();
    let task_runtime = harness.runtime.clone();
    let task_session = session_id.clone();
    let task = harness.runtime.stream_tasks.spawn(&session_id, async move {
        let mut stream = futures::stream::poll_fn(move |_| {
            task_entered.cancel();
            let _released = release_rx.recv_timeout(kraai_test_support::HANG_TIMEOUT);
            task_runtime.send_event(Event::HistoryUpdated {
                session_id: task_session.clone(),
            });
            std::task::Poll::Ready(Some(()))
        });
        let _ = futures::StreamExt::next(&mut stream).await;
    });
    hang_guard(entered.cancelled()).await?;
    let runtime = harness.runtime.clone();
    let shutdown = runtime.stop_active_work();
    tokio::pin!(shutdown);
    let shutdown_pending = poll!(&mut shutdown).is_pending();
    let stream_finished_early = task.is_finished();

    release_tx.send(()).expect("stream poll is blocked");
    if shutdown_pending {
        hang_guard(shutdown).await?;
    }
    let _task_result = hang_guard(task).await?;
    assert!(shutdown_pending);
    assert!(!stream_finished_early);
    assert!(harness.runtime.stream_tasks.is_empty());
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn shutdown_cancels_a_running_script_and_waits_for_its_finalization() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let runtime = harness.runtime.clone();
    let (generation, cancellation, mailbox) = runtime
        .with_session_actor(&session_id, |actor| {
            Box::pin(async move {
                let (generation, cancellation) = actor.begin_test_script();
                (generation, cancellation, actor.test_mailbox())
            })
        })
        .await;
    let mut events = harness.runtime.event_tx.subscribe();
    let shutdown = runtime.stop_active_work();
    tokio::pin!(shutdown);
    assert!(poll!(&mut shutdown).is_pending());
    hang_guard(cancellation.cancelled()).await?;
    assert!(poll!(&mut shutdown).is_pending());

    let _ = mailbox.send(crate::runtime::session::SessionMessage::ScriptEnded {
        generation,
        result: Err(color_eyre::eyre::eyre!("script cancelled")),
    });
    hang_guard(shutdown).await?;
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event.event,
            Event::ContinuationFailed { session_id: actual, .. } if actual == session_id
        ))
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn session_commands_after_shutdown_cannot_start_a_new_stream() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let agent = &harness.runtime.agent_manager;
    let request = agent
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    agent.complete_message(&request.message_id).await?;
    harness.runtime.stop_active_work().await;
    let event_sequence = harness.runtime.event_tx.latest_sequence();
    assert!(
        harness
            .handle
            .send_message(
                session_id.clone(),
                "again".into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
            )
            .await
            .is_err()
    );
    let (response, outcome) = tokio::sync::oneshot::channel();
    harness.runtime.sessions.send(
        &harness.runtime,
        &session_id,
        crate::runtime::session::SessionMessage::Send {
            message: "again".into(),
            selection: kraai_types::ModelSelection {
                provider_id: kraai_types::ProviderId::new("mock"),
                model_id: kraai_types::ModelId::new("mock-model"),
                options: Default::default(),
            },
            response,
        },
    );
    assert!(outcome.await?.is_err());
    assert_eq!(harness.runtime.event_tx.latest_sequence(), event_sequence);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn slow_snapshot_history_does_not_block_commands_or_state_events() -> Result<()> {
    let data_dir =
        std::env::temp_dir().join(format!("kraai-slow-history-{}", ulid::Ulid::generate()));
    tokio::fs::create_dir_all(&data_dir).await?;
    let store = Arc::new(PausedMessageStore {
        inner: SqliteMessageStore::new(&data_dir),
        pause: AtomicBool::new(false),
        pause_completed_script: AtomicBool::new(false),
        fail_script_result_save: AtomicBool::new(false),
        persist_failed_script_result: false,
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let harness = RuntimeTestHarness::new_with_message_store(
        kraai_provider_core::ProviderManager::new(),
        Some(store.clone()),
    )
    .await
    .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let message_id = MessageId::new(ulid::Ulid::generate());
    harness
        .runtime
        .agent_manager
        .add_script_result_to_history(
            &session_id,
            message_id.clone(),
            "test-profile".into(),
            kraai_types::ToolCallId::new("test-call"),
            "old history".into(),
            kraai_execution_types::ScriptExecutionOutcome {
                status: kraai_execution_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
        )
        .await?;
    store.pause.store(true, Ordering::SeqCst);
    let snapshot_task = tokio::spawn({
        let handle = harness.handle.clone();
        let session_id = session_id.clone();
        async move { handle.get_session_snapshot(session_id).await }
    });
    hang_guard(store.entered.notified()).await?;
    hang_guard(harness.handle.list_provider_definitions()).await??;
    let guard = {
        let barrier = harness.runtime.sessions.barrier(&session_id);
        hang_guard(barrier.read_owned()).await?
    };
    harness
        .runtime
        .send_event(Event::HistoryUpdated { session_id });
    let later_sequence = harness.runtime.event_tx.latest_sequence();
    drop(guard);
    store.release.notify_one();
    let snapshot = hang_guard(snapshot_task).await???;
    assert!(snapshot.event_sequence < later_sequence);
    assert_eq!(
        snapshot
            .history
            .get(&message_id)
            .expect("captured history")
            .content,
        kraai_types::ConversationItem::ScriptResult {
            outcome: kraai_execution_types::ScriptExecutionOutcome {
                status: kraai_execution_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
            call_id: kraai_types::ToolCallId::new("test-call"),
            output: "old history".into()
        }
    );
    harness.shutdown().await;
    tokio::fs::remove_dir_all(data_dir).await?;
    Ok(())
}

struct PausedMessageStore {
    inner: SqliteMessageStore,
    pause: AtomicBool,
    pause_completed_script: AtomicBool,
    fail_script_result_save: AtomicBool,
    persist_failed_script_result: bool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl MessageStore for PausedMessageStore {
    fn sqlite_database(&self) -> Option<kraai_persistence::SqliteDatabase> {
        self.inner.sqlite_database()
    }
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        if self.pause.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.get(id).await
    }
    async fn save(&self, message: &Message) -> Result<()> {
        if matches!(
            &message.content,
            kraai_types::ConversationItem::ScriptResult { .. }
        ) && self.fail_script_result_save.swap(false, Ordering::SeqCst)
        {
            if self.persist_failed_script_result {
                self.inner.save(message).await?;
            }
            return Err(color_eyre::eyre::eyre!(
                "injected cancelled result save failure"
            ));
        }
        if message.status == kraai_types::MessageStatus::Complete
            && matches!(&message.content, kraai_types::ConversationItem::Assistant { items }
                if items.iter().any(|item| matches!(item, kraai_types::AssistantItem::ScriptCall { .. })))
            && self.pause_completed_script.swap(false, Ordering::SeqCst)
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.inner.save(message).await
    }
    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.inner.delete(id).await
    }
    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }
    async fn list_ids(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_ids().await
    }
}
