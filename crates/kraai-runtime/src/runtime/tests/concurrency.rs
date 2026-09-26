use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use color_eyre::eyre::Result;
use futures::{StreamExt, poll};
use kraai_persistence::{FileMessageStore, MessageStore};
use kraai_types::{Message, MessageId};
use tokio_util::sync::CancellationToken;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::runtime::core::{ActiveScriptTask, ActiveStream};
use crate::{ContinueSessionOutcome, Event, SessionActivity};

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
        Vec::new()
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: kraai_provider_core::ModelConfig) -> Result<()> {
        Ok(())
    }

    fn script_tool_transport(
        &self,
        _model_id: &kraai_types::ModelId,
    ) -> kraai_provider_core::ScriptToolTransport {
        kraai_provider_core::ScriptToolTransport::NativeCustom
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
async fn continuation_cannot_overtake_script_approval_during_stream_completion() -> Result<()> {
    let data_dir =
        std::env::temp_dir().join(format!("kraai-script-handoff-{}", ulid::Ulid::generate()));
    let store = Arc::new(PausedMessageStore {
        inner: FileMessageStore::new(&data_dir),
        pause: AtomicBool::new(false),
        pause_completed_script: AtomicBool::new(true),
        fail_script_result_save: AtomicBool::new(false),
        persist_failed_script_result: false,
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
            hold_after_call: false,
            text_before_call: None,
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
            String::from("change it"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(1), received.recv())
        .await?
        .expect("initial provider request");
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified()).await?;

    let handle = harness.handle.clone();
    let continuation = handle.continue_session(session_id.clone());
    tokio::pin!(continuation);
    let early_result = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let std::task::Poll::Ready(result) = poll!(&mut continuation) {
                break Some(result);
            }
            if harness.runtime.agent_manager.try_read().is_err() {
                break None;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    store.release.notify_one();
    let outcome = match early_result {
        Some(result) => result?,
        None => tokio::time::timeout(Duration::from_secs(1), &mut continuation).await??,
    };
    let premature_request = if outcome == ContinueSessionOutcome::Started {
        Some(
            tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await?
                .expect("continued provider request"),
        )
    } else {
        None
    };
    harness
        .events
        .wait_for("script approval after handoff", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ScriptApprovalRequested { session_id: actual, .. } if actual == &session_id)
            })
        })
        .await;
    if outcome == ContinueSessionOutcome::NothingToContinue {
        let pending = harness
            .handle
            .get_pending_script(session_id.clone())
            .await?
            .expect("pending script approval");
        harness
            .handle
            .deny_script(session_id.clone(), pending.execution_id)
            .await?;
        let request = tokio::time::timeout(Duration::from_secs(1), received.recv())
            .await?
            .expect("automatic continuation after denial");
        assert!(request.messages.iter().any(|message| {
            matches!(message, kraai_types::ConversationItem::ScriptResult { call_id, .. }
                if call_id.as_str() == "handoff-call")
        }));
        assert_eq!(
            harness.handle.continue_session(session_id.clone()).await?,
            ContinueSessionOutcome::NothingToContinue
        );
        assert!(received.try_recv().is_err());
    }
    harness.shutdown().await;
    tokio::fs::remove_dir_all(data_dir).await?;
    assert_eq!(
        outcome,
        ContinueSessionOutcome::NothingToContinue,
        "continuation bypassed script approval; provider history: {:?}",
        premature_request.map(|request| request.messages)
    );
    Ok(())
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
    let store = Arc::new(PausedMessageStore {
        inner: FileMessageStore::new(&data_dir),
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
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(1), received.recv())
        .await?
        .expect("first provider request");
    harness
        .events
        .wait_for("script call before usage drain completes", |events| {
            events.iter().any(|event| {
                matches!(event, Event::StreamChunk { session_id: id, chunk, .. }
                if id == &session_id && chunk.contains("<tool_call>"))
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
        assert_eq!(
            harness.handle.continue_session(session_id.clone()).await?,
            ContinueSessionOutcome::NothingToContinue
        );
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
        )
        .await?;
    let request = tokio::time::timeout(Duration::from_secs(1), received.recv())
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
            if let kraai_types::ConversationItem::ScriptResult { call_id, output } = item
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
    let mut on_disk: Vec<_> = store.list_all_on_disk().await?.into_iter().collect();
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
async fn handoff_defers_queue_drain_without_blocking_another_session() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![
        vec![ScriptedChunk::plain("other session complete")],
        vec![ScriptedChunk::plain("queued message complete")],
    ])
    .await
    .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let other_session = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut runtime = harness.runtime.clone();
    runtime.queue_drains = Arc::default();
    let preparation = runtime.session_preparations.begin(&session_id).await;
    assert!(matches!(
        runtime
            .handle_send_message(
                session_id.clone(),
                "queued".into(),
                kraai_types::ModelId::new("mock-model"),
                kraai_types::ProviderId::new("mock"),
            )
            .await?,
        crate::SubmitMessageOutcome::Queued { position: 1 }
    ));
    let (_commands, mut receiver) = tokio::sync::mpsc::channel(1);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut receiver))
            .await?,
        Some(crate::handle::Command::StartQueuedMessages { session_id: id }) if id == session_id
    ));
    runtime
        .handle_start_queued_messages(session_id.clone())
        .await;
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            harness.handle.send_message(
                other_session,
                "independent".into(),
                "mock-model".into(),
                "mock".into(),
            ),
        )
        .await??,
        crate::SubmitMessageOutcome::Started { .. }
    ));
    drop(preparation);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), runtime.next_command(&mut receiver))
            .await?,
        Some(crate::handle::Command::StartQueuedMessages { session_id: id }) if id == session_id
    ));
    runtime
        .handle_start_queued_messages(session_id.clone())
        .await;
    harness
        .events
        .wait_for("deferred message started", |events| {
            events.iter().any(|event| {
            matches!(event, Event::StreamStart { session_id: id, .. } if id == &session_id)
        })
        })
        .await;
    assert!(runtime.take_queued_messages(&session_id).await.is_empty());
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn automatic_continuation_waits_for_handoff() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain("continued")]])
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let request = harness
        .runtime
        .agent_manager
        .write()
        .await
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
        )
        .await?;
    harness
        .runtime
        .agent_manager
        .read()
        .await
        .complete_message(&request.message_id)
        .await?;
    let preparation = harness
        .runtime
        .session_preparations
        .begin(&session_id)
        .await;
    harness.runtime.spawn_continuation(session_id.clone());
    tokio::time::timeout(Duration::from_secs(1), async {
        while harness.runtime.session_state_barrier.try_write().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(
        harness.handle.continue_session(session_id.clone()).await?,
        ContinueSessionOutcome::NothingToContinue
    );
    drop(preparation);
    let events = harness
        .events
        .wait_for("automatic continuation after handoff", |events| {
            events.iter().any(|event| {
            matches!(event, Event::StreamComplete { session_id: id, .. } if id == &session_id)
        })
        })
        .await;
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                matches!(event, Event::StreamStart { session_id: id, .. } if id == &session_id)
            })
            .count(),
        1
    );
    harness.shutdown().await;
    Ok(())
}

async fn wait_for_snapshot_writer(
    snapshot: impl std::future::Future + Send,
    barrier: &tokio::sync::RwLock<()>,
) -> Result<()> {
    tokio::pin!(snapshot);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            assert!(poll!(&mut snapshot).is_pending());
            if barrier.try_read().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
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
        .write()
        .await
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
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
        let snapshot = harness.runtime.build_session_snapshot(&session_id).await?;
        assert!(snapshot.session.is_running);
        assert_eq!(snapshot.session.is_streaming, is_streaming);
        if is_streaming {
            harness
                .runtime
                .agent_manager
                .write()
                .await
                .complete_message(&request.message_id)
                .await?;
        }
    }

    harness
        .runtime
        .agent_manager
        .write()
        .await
        .clear_active_turn(&session_id);
    let sessions = harness.handle.list_sessions().await?;
    assert!(sessions.iter().all(|session| !session.is_running));
    let snapshot = harness.runtime.build_session_snapshot(&session_id).await?;
    assert!(!snapshot.session.is_running);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn stream_start_events_precede_a_snapshot_queued_during_preparation() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mutation = harness.runtime.session_state_barrier.read().await;
    let mut agent = harness.runtime.agent_manager.write().await;
    let mut request = agent
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
        )
        .await?;
    let providers = agent.cloned_provider_manager();
    drop(agent);
    request.context_notifications = vec!["missing file automatically unpinned".into()];
    let message_id = request.message_id.to_string();
    let mut events = harness.runtime.event_tx.subscribe();
    let runtime = harness.runtime.clone();
    let snapshot = runtime.build_session_snapshot(&session_id);
    tokio::pin!(snapshot);
    wait_for_snapshot_writer(&mut snapshot, &harness.runtime.session_state_barrier).await?;

    harness
        .runtime
        .start_stream_job(
            super::super::streaming::StreamJobKind::Initial,
            session_id.clone(),
            providers,
            request,
        )
        .await;
    // Neither the spawned provider task nor a snapshot may separate registration
    // from its initial events, even when preparation produced notifications.
    let context = events.try_recv()?;
    assert!(
        matches!(context.event, Event::ContextStateChanged { notifications, .. } if notifications.len() == 1)
    );
    let timing = events.try_recv()?;
    let Event::TurnTimingChanged { timer, .. } = timing.event else {
        panic!("expected runtime turn timing");
    };
    let start = events.try_recv()?;
    assert!(
        matches!(start.event, Event::StreamStart { message_id: actual, .. } if actual == message_id)
    );
    assert!(context.sequence < start.sequence);
    drop(mutation);
    let snapshot = tokio::time::timeout(Duration::from_secs(1), snapshot).await??;
    assert!(snapshot.event_sequence >= start.sequence);
    assert_eq!(snapshot.activity, SessionActivity::Streaming);
    assert_eq!(snapshot.turn_timer, timer);
    assert!(timer.elapsed(std::time::Instant::now()).is_some());
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn cancellation_finishes_with_a_queued_snapshot_and_stays_active_until_finalized()
-> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let cancellation = CancellationToken::new();
    let completion = CancellationToken::new();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let runtime = harness.runtime.clone();
    let task_session = session_id.clone();
    let completion_guard = completion.clone().drop_guard();
    let join_handle = tokio::spawn(async move {
        let _completion_guard = completion_guard;
        finish_rx.await.expect("release finalization");
        let _guard = runtime.session_state_barrier.read().await;
        runtime
            .active_script_tasks
            .lock()
            .await
            .remove(&task_session);
        runtime.send_event(Event::HistoryUpdated {
            session_id: task_session,
        });
    });
    harness.runtime.active_script_tasks.lock().await.insert(
        session_id.clone(),
        ActiveScriptTask {
            cancellation: cancellation.clone(),
            completion: completion.clone(),
            join_handle,
        },
    );

    let (response, result) = tokio::sync::oneshot::channel();
    let cancel_runtime = harness.runtime.clone();
    let cancel = cancel_runtime.handle_command(crate::handle::Command::CancelStream {
        session_id: session_id.clone(),
        response,
    });
    tokio::pin!(cancel);
    assert!(poll!(&mut cancel).is_pending());
    assert!(cancellation.is_cancelled());

    // Force the snapshot writer to queue before the completion path requests a reader.
    let mutation = harness.runtime.session_state_barrier.read().await;
    let snapshot = cancel_runtime.build_session_snapshot(&session_id);
    tokio::pin!(snapshot);
    wait_for_snapshot_writer(&mut snapshot, &harness.runtime.session_state_barrier).await?;
    finish_tx.send(()).expect("finish task still running");
    tokio::task::yield_now().await;
    drop(mutation);
    let snapshot = tokio::time::timeout(Duration::from_secs(1), snapshot).await??;
    assert_eq!(snapshot.activity, SessionActivity::ExecutingScript);
    tokio::time::timeout(Duration::from_secs(1), &mut cancel).await??;
    assert!(result.await??);
    assert!(completion.is_cancelled());
    assert!(harness.runtime.active_script_tasks.lock().await.is_empty());
    harness.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_synchronous_stream_poll_to_return() -> Result<()> {
    assert_shutdown_waits_for_synchronous_stream(false).await?;
    assert_shutdown_waits_for_synchronous_stream(true).await
}

async fn assert_shutdown_waits_for_synchronous_stream(cancel_before_shutdown: bool) -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let entered = CancellationToken::new();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task_entered = entered.clone();
    let task_runtime = harness.runtime.clone();
    let task_session = session_id.clone();
    let task = harness.runtime.stream_tasks.spawn(async move {
        let mut stream = futures::stream::poll_fn(move |_| {
            task_entered.cancel();
            let _released = release_rx.recv_timeout(Duration::from_secs(5));
            task_runtime.send_event(Event::HistoryUpdated {
                session_id: task_session.clone(),
            });
            std::task::Poll::Ready(Some(()))
        });
        let _ = futures::StreamExt::next(&mut stream).await;
    });
    harness.runtime.active_streams.lock().await.insert(
        session_id.clone(),
        ActiveStream {
            message_id: MessageId::new("synchronous-stream"),
            abort_handle: task.abort_handle(),
        },
    );
    tokio::time::timeout(Duration::from_secs(1), entered.cancelled()).await?;
    if cancel_before_shutdown {
        harness.runtime.cancel_stream(session_id).await?;
        assert!(harness.runtime.active_streams.lock().await.is_empty());
    }
    let runtime = harness.runtime.clone();
    let shutdown = runtime.stop_active_work();
    tokio::pin!(shutdown);
    let shutdown_pending = poll!(&mut shutdown).is_pending();
    let stream_finished_early = task.is_finished();

    release_tx.send(()).expect("stream poll is blocked");
    if shutdown_pending {
        tokio::time::timeout(Duration::from_secs(1), shutdown).await?;
    }
    let _task_result = tokio::time::timeout(Duration::from_secs(1), task).await?;
    assert!(shutdown_pending);
    assert!(!stream_finished_early);
    assert!(harness.runtime.stream_tasks.is_empty());
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn shutdown_waits_for_script_finalization_after_task_removal() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let (removed_tx, removed_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let completion = CancellationToken::new();
    let completion_guard = completion.clone().drop_guard();
    let runtime = harness.runtime.clone();
    let task_session = session_id.clone();
    let join_handle = tokio::spawn(async move {
        let _completion_guard = completion_guard;
        start_rx.await.expect("start finalization");
        let _guard = runtime.session_state_barrier.read().await;
        runtime
            .active_script_tasks
            .lock()
            .await
            .remove(&task_session);
        removed_tx.send(()).expect("report task removal");
        finish_rx.await.expect("release finalization");
        runtime.send_event(Event::HistoryUpdated {
            session_id: task_session,
        });
    });
    harness.runtime.active_script_tasks.lock().await.insert(
        session_id.clone(),
        ActiveScriptTask {
            cancellation: CancellationToken::new(),
            completion: completion.clone(),
            join_handle,
        },
    );
    start_tx.send(()).expect("registered task is waiting");
    tokio::time::timeout(Duration::from_secs(1), removed_rx).await??;
    let mut events = harness.runtime.event_tx.subscribe();
    let shutdown_runtime = harness.runtime.clone();
    let shutdown = shutdown_runtime.stop_active_work();
    tokio::pin!(shutdown);
    assert!(poll!(&mut shutdown).is_pending());
    assert!(!completion.is_cancelled());

    finish_tx.send(()).expect("finalization is waiting");
    tokio::time::timeout(Duration::from_secs(1), shutdown).await?;
    assert!(completion.is_cancelled());
    assert!(matches!(
        events.try_recv()?.event,
        Event::HistoryUpdated { session_id: actual } if actual == session_id
    ));
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn continuation_queued_behind_shutdown_cannot_start_a_new_stream() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut agent = harness.runtime.agent_manager.write().await;
    let request = agent
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
        )
        .await?;
    agent.complete_message(&request.message_id).await?;
    drop(agent);
    let event_sequence = harness.runtime.event_tx.latest_sequence();
    let runtime = harness.runtime.clone();
    let mutation = runtime.session_state_barrier.read().await;
    let shutdown = runtime.stop_active_work();
    tokio::pin!(shutdown);
    assert!(poll!(&mut shutdown).is_pending());
    let continuation = async {
        let _guard = runtime.session_state_barrier.read().await;
        runtime.start_continuation(session_id.clone()).await
    };
    tokio::pin!(continuation);
    assert!(poll!(&mut continuation).is_pending());
    drop(mutation);

    let ((), outcome) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(shutdown, continuation)
    })
    .await?;
    assert_eq!(outcome?, ContinueSessionOutcome::NothingToContinue);
    assert!(harness.runtime.active_streams.lock().await.is_empty());
    assert_eq!(harness.runtime.event_tx.latest_sequence(), event_sequence);
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn slow_snapshot_history_does_not_block_commands_or_state_events() -> Result<()> {
    let data_dir =
        std::env::temp_dir().join(format!("kraai-slow-history-{}", ulid::Ulid::generate()));
    let store = Arc::new(PausedMessageStore {
        inner: FileMessageStore::new(&data_dir),
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
        .write()
        .await
        .add_script_result_to_history(
            &session_id,
            message_id.clone(),
            "test-profile".into(),
            kraai_types::ToolCallId::new("test-call"),
            "old history".into(),
        )
        .await?;
    store.pause.store(true, Ordering::SeqCst);
    let snapshot_task = tokio::spawn({
        let handle = harness.handle.clone();
        let session_id = session_id.clone();
        async move { handle.get_session_snapshot(session_id).await }
    });
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified()).await?;
    tokio::time::timeout(
        Duration::from_secs(1),
        harness.handle.list_provider_definitions(),
    )
    .await??;
    let guard = tokio::time::timeout(
        Duration::from_secs(1),
        harness.runtime.session_state_barrier.read(),
    )
    .await?;
    harness
        .runtime
        .send_event(Event::HistoryUpdated { session_id });
    let later_sequence = harness.runtime.event_tx.latest_sequence();
    drop(guard);
    store.release.notify_one();
    let snapshot = tokio::time::timeout(Duration::from_secs(1), snapshot_task).await???;
    assert!(snapshot.event_sequence < later_sequence);
    assert_eq!(
        snapshot
            .history
            .get(&message_id)
            .expect("captured history")
            .content,
        kraai_types::ConversationItem::ScriptResult {
            call_id: kraai_types::ToolCallId::new("test-call"),
            output: "old history".into()
        }
    );
    harness.shutdown().await;
    tokio::fs::remove_dir_all(data_dir).await?;
    Ok(())
}

struct PausedMessageStore {
    inner: FileMessageStore,
    pause: AtomicBool,
    pause_completed_script: AtomicBool,
    fail_script_result_save: AtomicBool,
    persist_failed_script_result: bool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl MessageStore for PausedMessageStore {
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
    async fn unload(&self, id: &MessageId) {
        self.inner.unload(id).await;
    }
    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.inner.delete(id).await
    }
    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }
    async fn list_all_on_disk(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_all_on_disk().await
    }
    async fn list_hot(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_hot().await
    }
}
