use color_eyre::eyre::Result;
use futures::poll;
use kraai_persistence::{FileRequestUsageStore, RequestUsageStore};
use kraai_types::{ModelId, ProviderId, TokenUsage};
use std::{sync::Arc, time::Duration};
use tokio::sync::oneshot;

use super::harness::{RuntimeTestHarness, create_session_with_profile};
use crate::runtime::core::ActiveStream;

#[tokio::test]
async fn cancellation_waits_for_received_usage_to_be_persisted() -> Result<()> {
    assert_usage_survives_abort(false).await
}

#[tokio::test]
async fn shutdown_waits_for_received_usage_to_be_persisted() -> Result<()> {
    assert_usage_survives_abort(true).await
}

#[tokio::test]
async fn cancelled_unpolled_auxiliary_commit_retains_session_and_shutdown_ownership() -> Result<()>
{
    let directory =
        std::env::temp_dir().join(format!("kraai-auxiliary-usage-{}", ulid::Ulid::generate()));
    tokio::fs::create_dir_all(&directory).await?;
    let tasks = super::super::stream_tasks::StreamTasks::default();
    let barrier = Arc::new(tokio::sync::RwLock::new(()));
    let event_tx = crate::handle::RuntimeEventSender::new(8);
    let mut events = event_tx.subscribe();
    let recorder = kraai_agent::AuxiliaryUsageRecorder {
        store: Arc::new(FileRequestUsageStore::new(&directory)),
        session_id: String::from("session"),
        barrier: Some(barrier.clone()),
        on_usage: Some(super::super::stream_driver::auxiliary_usage_observer(
            event_tx,
            String::from("session"),
            tasks.session_token("session"),
        )),
    };
    let providers = kraai_provider_core::ProviderManager::new();
    let provider = ProviderId::new("provider");
    let model = ModelId::new("model");
    {
        let starting = recorder.start(&providers, &provider, &model, "compaction");
        tokio::pin!(starting);
        assert!(poll!(&mut starting).is_pending());
    }
    let session = tasks.wait_session("session");
    tokio::pin!(session);
    tasks.close();
    let all = tasks.wait();
    tokio::pin!(all);
    assert!(poll!(&mut session).is_pending());
    assert!(poll!(&mut all).is_pending());
    assert!(barrier.try_write().is_err());
    assert!(events.try_recv().is_err());
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(session, all);
    })
    .await?;
    assert!(barrier.try_write().is_ok());
    let crate::api::Event::RequestUsageUpdated {
        session_id,
        request,
    } = events.try_recv()?.event
    else {
        return Err(color_eyre::eyre::eyre!(
            "Expected persisted auxiliary usage notification"
        ));
    };
    assert_eq!(session_id, "session");
    let reopened = FileRequestUsageStore::new(&directory);
    assert_eq!(
        reopened.load("session").await?.get(&request.message_id),
        Some(request.as_ref())
    );
    reopened.delete("session").await?;
    assert!(reopened.load("session").await?.is_empty());
    tokio::fs::remove_dir_all(directory).await?;
    Ok(())
}

async fn assert_usage_survives_abort(shutdown: bool) -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
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
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    let message_id = request.message_id;
    let manager = Arc::clone(&harness.runtime.agent_manager);
    let task_message_id = message_id.clone();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (saved_tx, saved_rx) = oneshot::channel();
    let task = harness.runtime.stream_tasks.spawn(&session_id, async move {
        let agent = manager.read().await;
        started_tx.send(()).unwrap();
        release_rx.await.unwrap();
        let request = agent
            .set_streaming_message_usage(
                &task_message_id,
                TokenUsage {
                    total_tokens: 42,
                    input_tokens: 30,
                    output_tokens: 12,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        saved_tx.send(request).unwrap();
        drop(agent);
        std::future::pending::<()>().await;
    });
    started_rx.await?;
    harness.runtime.active_streams.lock().await.insert(
        session_id.clone(),
        ActiveStream {
            message_id: message_id.clone(),
            abort_handle: task.abort_handle(),
        },
    );
    let (cancelled, saved) = {
        let cancellation = async {
            if shutdown {
                harness.runtime.stop_active_work().await;
                Ok(true)
            } else {
                harness.runtime.cancel_stream(session_id.clone()).await
            }
        };
        tokio::pin!(cancellation);
        assert!(poll!(&mut cancellation).is_pending());
        assert!(!task.is_finished());
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(cancellation, saved_rx)
        })
        .await?
    };
    assert!(cancelled?);
    let saved = saved?;
    assert!(task.await.unwrap_err().is_cancelled());
    let requests = FileRequestUsageStore::new(&harness.data_dir)
        .load(&session_id)
        .await?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests.get(&message_id), Some(&saved));
    harness.shutdown().await;
    Ok(())
}
