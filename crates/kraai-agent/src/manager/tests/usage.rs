use super::super::*;
use super::common::{cleanup_dir, test_manager};
use color_eyre::eyre::Result;

#[tokio::test]
async fn request_cost_survives_empty_response_cancellation_and_store_reload() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let pending = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.record_request_started(&pending.message_id).await?;
    let initial = manager
        .capture_session_snapshot(&session_id)
        .await?
        .load()
        .await?;
    assert!(
        initial
            .requests
            .get(&pending.message_id)
            .is_some_and(|request| request.usage.is_none())
    );
    manager
        .record_request_attempt(&pending.message_id, 1)
        .await?;
    let usage = TokenUsage {
        input_tokens: 10,
        total_tokens: 10,
        cost: Some(kraai_types::RequestCost {
            amount: kraai_types::Usd(12_300_000),
            source: "openrouter".into(),
            rates: None,
            priced_at: 1,
            upstream: None,
        }),
        ..Default::default()
    };
    assert!(
        manager
            .set_streaming_message_usage(&pending.message_id, usage.clone())
            .await?
            .is_some()
    );
    manager
        .cancel_streaming_message(&pending.message_id)
        .await?;
    assert!(
        !manager
            .get_chat_history(&session_id)
            .await?
            .contains_key(&pending.message_id)
    );
    let store = kraai_persistence::RequestUsageStore::new(&data_dir);
    let requests = store.load(&session_id).await?;
    assert_eq!(
        requests
            .get(&pending.message_id)
            .and_then(|request| request.usage.as_ref()),
        Some(&usage)
    );
    let snapshot = manager
        .capture_session_snapshot(&session_id)
        .await?
        .load()
        .await?;
    assert_eq!(snapshot.requests, requests);
    assert_eq!(
        requests
            .get(&pending.message_id)
            .map(|request| request.unpriced_attempts),
        Some(1)
    );
    assert!(store.load("../escape").await.is_err());
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn aborted_request_without_usage_remains_unknown() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let pending = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
        )
        .await?;
    manager.record_request_started(&pending.message_id).await?;
    manager.abort_streaming_message(&pending.message_id).await?;
    let requests = kraai_persistence::RequestUsageStore::new(&data_dir)
        .load(&session_id)
        .await?;
    assert!(
        requests
            .get(&pending.message_id)
            .is_some_and(|request| request.usage.is_none())
    );
    cleanup_dir(data_dir).await;
    Ok(())
}
