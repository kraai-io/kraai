use super::super::*;
use super::common::{cleanup_dir, test_manager};
use kraai_persistence::RequestUsageStore;
use std::sync::atomic::{AtomicBool, Ordering};

struct ControlledUsageStore {
    inner: Arc<dyn RequestUsageStore>,
    reject_writes: AtomicBool,
}

#[async_trait::async_trait]
impl RequestUsageStore for ControlledUsageStore {
    async fn save(
        &self,
        session_id: &SessionId,
        request: &kraai_types::RequestUsage,
    ) -> Result<()> {
        if self.reject_writes.load(Ordering::Acquire) {
            return Err(eyre!("usage storage unavailable"));
        }
        self.inner.save(session_id, request).await
    }

    async fn delete(&self, session_id: &SessionId) -> Result<()> {
        self.inner.delete(session_id).await
    }

    async fn load(
        &self,
        session_id: &SessionId,
    ) -> Result<BTreeMap<MessageId, kraai_types::RequestUsage>> {
        self.inner.load(session_id).await
    }
}

#[tokio::test]
async fn rejected_usage_writes_leave_stream_state_unchanged_and_can_be_retried() {
    let (mut manager, data_dir) = test_manager().await;
    let store = Arc::new(ControlledUsageStore {
        inner: manager.usage_store.clone(),
        reject_writes: AtomicBool::new(true),
    });
    manager.usage_store = store.clone();
    let session_id = manager.create_session().await.unwrap();
    let pending = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    let usage = TokenUsage {
        input_tokens: 10,
        total_tokens: 10,
        ..Default::default()
    };
    assert!(
        manager
            .record_request_started(&pending.message_id)
            .await
            .is_err()
    );
    assert!(
        manager
            .record_request_attempt(&pending.message_id, 2)
            .await
            .is_err()
    );
    assert!(
        manager
            .set_streaming_message_usage(&pending.message_id, usage.clone())
            .await
            .is_err()
    );
    assert!(store.load(&session_id).await.unwrap().is_empty());
    assert!(
        manager
            .get_chat_history(&session_id)
            .await
            .unwrap()
            .get(&pending.message_id)
            .and_then(|message| message.generation.as_ref())
            .is_some_and(|generation| generation.usage.is_none())
    );

    store.reject_writes.store(false, Ordering::Release);
    let initial = manager
        .record_request_started(&pending.message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial.unpriced_attempts, 0);
    manager
        .record_request_attempt(&pending.message_id, 2)
        .await
        .unwrap();
    manager
        .set_streaming_message_usage(&pending.message_id, usage.clone())
        .await
        .unwrap();
    let requests = manager
        .capture_session_snapshot(&session_id)
        .await
        .unwrap()
        .load()
        .await
        .unwrap()
        .requests;
    let saved = requests.get(&pending.message_id).unwrap();
    assert_eq!(saved.unpriced_attempts, 2);
    assert_eq!(saved.usage.as_ref(), Some(&usage));
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn request_cost_survives_empty_response_cancellation_and_store_reload() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let pending = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .record_request_started(&pending.message_id)
        .await
        .unwrap();
    let initial = manager
        .capture_session_snapshot(&session_id)
        .await
        .unwrap()
        .load()
        .await
        .unwrap();
    assert!(
        initial
            .requests
            .get(&pending.message_id)
            .is_some_and(|request| request.usage.is_none())
    );
    manager
        .record_request_attempt(&pending.message_id, 1)
        .await
        .unwrap();
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
            .await
            .unwrap()
            .is_some()
    );
    manager
        .cancel_streaming_message(&pending.message_id)
        .await
        .unwrap();
    assert!(
        !manager
            .get_chat_history(&session_id)
            .await
            .unwrap()
            .contains_key(&pending.message_id)
    );
    let store = kraai_persistence::SqliteRequestUsageStore::new(&data_dir);
    let requests = store.load(&session_id).await.unwrap();
    assert_eq!(
        requests
            .get(&pending.message_id)
            .and_then(|request| request.usage.as_ref()),
        Some(&usage)
    );
    let snapshot = manager
        .capture_session_snapshot(&session_id)
        .await
        .unwrap()
        .load()
        .await
        .unwrap();
    assert_eq!(snapshot.requests, requests);
    assert_eq!(
        requests
            .get(&pending.message_id)
            .map(|request| request.unpriced_attempts),
        Some(1)
    );
    assert!(SessionId::try_new("../escape").is_err());
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn aborted_request_without_usage_remains_unknown() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let pending = manager
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .record_request_started(&pending.message_id)
        .await
        .unwrap();
    manager
        .abort_streaming_message(&pending.message_id)
        .await
        .unwrap();
    let requests = kraai_persistence::SqliteRequestUsageStore::new(&data_dir)
        .load(&session_id)
        .await
        .unwrap();
    assert!(
        requests
            .get(&pending.message_id)
            .is_some_and(|request| request.usage.is_none())
    );
    cleanup_dir(data_dir).await;
}

struct BlockedUsageStore {
    inner: Arc<dyn RequestUsageStore>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl RequestUsageStore for BlockedUsageStore {
    async fn save(
        &self,
        session_id: &SessionId,
        request: &kraai_types::RequestUsage,
    ) -> Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.save(session_id, request).await
    }

    async fn delete(&self, session_id: &SessionId) -> Result<()> {
        self.inner.delete(session_id).await
    }

    async fn load(
        &self,
        session_id: &SessionId,
    ) -> Result<BTreeMap<MessageId, kraai_types::RequestUsage>> {
        self.inner.load(session_id).await
    }
}

#[tokio::test]
async fn slow_usage_writes_do_not_hold_up_streamed_text() {
    let (mut manager, data_dir) = test_manager().await;
    let store = Arc::new(BlockedUsageStore {
        inner: manager.usage_store.clone(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    manager.usage_store = store.clone();
    let mut messages = Vec::new();
    for _ in 0..2 {
        let session_id = manager.create_session().await.unwrap();
        let pending = manager
            .prepare_start_stream(
                &session_id,
                "hello".into(),
                ModelId::new("mock-model"),
                ProviderId::new("mock"),
                Default::default(),
            )
            .await
            .unwrap();
        messages.push(pending.message_id);
    }
    let [slow, other] = messages.as_slice() else {
        unreachable!();
    };
    let usage = manager.record_request_started(slow);
    tokio::pin!(usage);
    tokio::select! {
        _ = &mut usage => panic!("usage write was not blocked"),
        () = store.entered.notified() => {}
    }
    for message in [slow, other] {
        kraai_test_support::hang_guard(manager.append_text_chunk(
            message,
            "text",
            AssistantPhase::FinalAnswer,
            "chunk",
        ))
        .await
        .unwrap()
        .expect("chunk recorded");
    }
    assert!(
        kraai_test_support::hang_guard(manager.publish_streaming_snapshots())
            .await
            .unwrap()
            .is_empty()
    );
    store.release.notify_one();
    assert!(usage.await.unwrap().is_some());
    cleanup_dir(data_dir).await;
}
