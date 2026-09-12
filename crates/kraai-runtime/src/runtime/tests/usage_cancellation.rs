use color_eyre::eyre::Result;
use futures::poll;
use kraai_persistence::RequestUsageStore;
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
    let task = tokio::spawn(async move {
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
    let requests = RequestUsageStore::new(&harness.data_dir)
        .load(&session_id)
        .await?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests.get(&message_id), Some(&saved));
    harness.shutdown().await;
    Ok(())
}
