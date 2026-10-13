use color_eyre::eyre::Result;
use futures::poll;
use kraai_persistence::{RequestUsageStore, SqliteRequestUsageStore};
use kraai_types::{ModelId, ProviderId, SessionId, TokenUsage};
use std::sync::Arc;

use super::harness::{RuntimeTestHarness, TEST_TIMEOUT, create_session_with_profile};

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
        store: Arc::new(SqliteRequestUsageStore::new(&directory)),
        session_id: SessionId::new("session"),
        barrier: Some(barrier.clone()),
        on_usage: Some(super::super::stream_driver::auxiliary_usage_observer(
            event_tx,
            SessionId::new("session"),
            tasks.session_token(&SessionId::new("session")),
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
    let session_id = SessionId::new("session");
    let session = tasks.wait_session(&session_id);
    tokio::pin!(session);
    tasks.close();
    let all = tasks.wait();
    tokio::pin!(all);
    assert!(poll!(&mut session).is_pending());
    assert!(poll!(&mut all).is_pending());
    assert!(barrier.try_write().is_err());
    assert!(events.try_recv().is_err());
    tokio::time::timeout(TEST_TIMEOUT, async {
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
    assert_eq!(session_id, SessionId::new("session"));
    let reopened = SqliteRequestUsageStore::new(&directory);
    assert_eq!(
        reopened
            .load(&SessionId::new("session"))
            .await?
            .get(&request.message_id),
        Some(request.as_ref())
    );
    reopened.delete(&SessionId::new("session")).await?;
    assert!(reopened.load(&SessionId::new("session")).await?.is_empty());
    tokio::fs::remove_dir_all(directory).await?;
    Ok(())
}

async fn assert_usage_survives_abort(shutdown: bool) -> Result<()> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut providers = kraai_provider_core::ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(super::shared_sessions::ControlledProvider {
            events: tokio::sync::Mutex::new(std::collections::VecDeque::from([receiver])),
        }),
    );
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "hello".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    let message_id = super::harness::started_stream(&harness.events.snapshot(), &session_id);
    sender.send(Ok(kraai_provider_core::ProviderStreamEvent::Usage(
        TokenUsage {
            total_tokens: 42,
            input_tokens: 30,
            output_tokens: 12,
            ..Default::default()
        },
    )))?;
    let events = harness
        .events
        .wait_for("received usage", |events| {
            events.iter().any(|event| {
                matches!(event, crate::Event::RequestUsageUpdated { request, .. } if request.usage.is_some())
            })
        })
        .await;
    let saved = events
        .iter()
        .find_map(|event| match event {
            crate::Event::RequestUsageUpdated { request, .. } if request.usage.is_some() => {
                Some(request.as_ref().clone())
            }
            _ => None,
        })
        .expect("usage event");
    let cancelled = if shutdown {
        tokio::time::timeout(TEST_TIMEOUT, harness.runtime.stop_active_work()).await?;
        true
    } else {
        tokio::time::timeout(
            TEST_TIMEOUT,
            harness.handle.cancel_stream(session_id.clone()),
        )
        .await??
    };
    assert!(cancelled);
    let requests = SqliteRequestUsageStore::new(&harness.data_dir)
        .load(&session_id)
        .await?;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests.get(&message_id), Some(&saved));
    drop(sender);
    harness.shutdown().await;
    Ok(())
}
