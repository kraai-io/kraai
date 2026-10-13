use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use kraai_persistence::{
    NewScriptExecution, PersistedScriptOutput, ScriptExecutionCompletion, ScriptExecutionRecord,
    ScriptExecutionStore,
};
use kraai_types::{ScriptExecutionId, ScriptOutputStream, SessionId};
use tokio::sync::Notify;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn deletion_waits_for_turn_preparation() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    super::harness::hold_preparation(&harness.runtime, &session_id).await;
    let deletion = {
        let handle = harness.handle.clone();
        let session_id = session_id.clone();
        async move { handle.delete_session(session_id).await }
    };
    tokio::pin!(deletion);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut deletion)
            .await
            .is_err()
    );
    super::harness::release_preparation(&harness.runtime, &session_id).await;
    tokio::time::timeout(Duration::from_secs(5), &mut deletion).await??;
    assert!(
        !harness
            .handle
            .list_sessions()
            .await?
            .iter()
            .any(|session| session.id == session_id)
    );
    harness.shutdown().await;
    Ok(())
}

struct BlockingPollProvider {
    entered: tokio_util::sync::CancellationToken,
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

#[async_trait::async_trait]
impl kraai_provider_core::Provider for BlockingPollProvider {
    fn get_provider_id(&self) -> kraai_types::ProviderId {
        kraai_types::ProviderId::new("mock")
    }

    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![super::harness::mock_model()]
    }

    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }

    async fn register_model(&mut self, _model: kraai_provider_core::ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &kraai_types::ModelId,
        _request: kraai_provider_core::ProviderRequest,
        _context: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<futures::stream::BoxStream<'static, Result<kraai_provider_core::ProviderStreamEvent>>>
    {
        let entered = self.entered.clone();
        let release = self
            .release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("one blocking stream");
        Ok(Box::pin(futures::stream::poll_fn(move |_| {
            entered.cancel();
            let _ = release.recv_timeout(Duration::from_secs(5));
            std::task::Poll::Ready(None)
        })))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deletion_waits_for_a_blocked_stream_to_stop_polling() -> Result<()> {
    let entered = tokio_util::sync::CancellationToken::new();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut providers = kraai_provider_core::ProviderManager::new();
    providers.register_provider(
        kraai_types::ProviderId::new("mock"),
        Box::new(BlockingPollProvider {
            entered: entered.clone(),
            release: std::sync::Mutex::new(Some(release_rx)),
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
    tokio::time::timeout(Duration::from_secs(5), entered.cancelled()).await?;
    let deletion = {
        let handle = harness.handle.clone();
        let session_id = session_id.clone();
        async move { handle.delete_session(session_id).await }
    };
    tokio::pin!(deletion);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut deletion)
            .await
            .is_err()
    );
    release_tx.send(())?;
    tokio::time::timeout(Duration::from_secs(5), &mut deletion).await??;
    assert!(
        !harness
            .handle
            .list_sessions()
            .await?
            .iter()
            .any(|session| session.id == session_id)
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn deleting_during_script_preparation_cannot_leave_an_orphan_approval() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![vec![ScriptedChunk::native_call(
        "call-1",
        "# timeout=30sec permissions=workspace-write\n'changed' | save result.txt",
    )]])
    .await
    .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let idle_session = create_session_with_profile(&harness.handle, "test-profile").await?;
    let store = Arc::new(PausedExecutionStore {
        inner: harness.runtime.execution_store.clone(),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let mut runtime = harness.runtime.clone();
    runtime.execution_store = store.clone();
    let (response, sent) = tokio::sync::oneshot::channel();
    runtime.dispatch(crate::handle::Command::SendMessage {
        session_id: session_id.clone(),
        message: "change it".into(),
        model_id: kraai_types::ModelId::new("mock-model"),
        provider_id: kraai_types::ProviderId::new("mock"),
        options: Default::default(),
        response,
    });
    sent.await??;
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified()).await?;
    harness.handle.delete_session(idle_session.clone()).await?;
    let deletion = {
        let handle = harness.handle.clone();
        let session_id = session_id.clone();
        async move { handle.delete_session(session_id).await }
    };
    tokio::pin!(deletion);
    assert!(futures::poll!(&mut deletion).is_pending());
    store.release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(1), &mut deletion).await?;
    harness.events.wait_for("script approval", |events| {
        events.iter().any(|event| {
            matches!(event, Event::ScriptApprovalRequested { session_id: id, .. } if id == &session_id)
        })
    }).await;
    let sessions = harness.handle.list_sessions().await?;
    let pending = harness
        .handle
        .get_pending_script(session_id.clone())
        .await?;
    harness.shutdown().await;
    let error = outcome.expect_err("deletion succeeded during script preparation");
    assert_eq!(error.kind, crate::RuntimeErrorKind::Conflict);
    assert!(sessions.iter().any(|session| session.id == session_id));
    assert!(!sessions.iter().any(|session| session.id == idle_session));
    assert!(pending.is_some());
    Ok(())
}

struct PausedExecutionStore {
    inner: Arc<dyn ScriptExecutionStore>,
    entered: Notify,
    release: Notify,
}

#[async_trait::async_trait]
impl ScriptExecutionStore for PausedExecutionStore {
    async fn create(&self, execution: NewScriptExecution) -> Result<ScriptExecutionRecord> {
        let record = self.inner.create(execution).await?;
        self.entered.notify_one();
        self.release.notified().await;
        Ok(record)
    }

    async fn get(&self, id: &ScriptExecutionId) -> Result<Option<ScriptExecutionRecord>> {
        self.inner.get(id).await
    }

    async fn list_for_session(&self, session_id: &SessionId) -> Result<Vec<ScriptExecutionRecord>> {
        self.inner.list_for_session(session_id).await
    }

    async fn list_all(&self) -> Result<Vec<ScriptExecutionRecord>> {
        self.inner.list_all().await
    }

    async fn read_source(&self, id: &ScriptExecutionId) -> Result<Vec<u8>> {
        self.inner.read_source(id).await
    }

    async fn read_output(&self, id: &ScriptExecutionId) -> Result<PersistedScriptOutput> {
        self.inner.read_output(id).await
    }

    async fn append_image(
        &self,
        id: &ScriptExecutionId,
        sequence: u64,
        image: kraai_types::ImageAttachment,
    ) -> Result<()> {
        self.inner.append_image(id, sequence, image).await
    }

    async fn mark_awaiting_approval(
        &self,
        id: &ScriptExecutionId,
    ) -> Result<ScriptExecutionRecord> {
        self.inner.mark_awaiting_approval(id).await
    }

    async fn mark_running(&self, id: &ScriptExecutionId) -> Result<ScriptExecutionRecord> {
        self.inner.mark_running(id).await
    }

    async fn append_output(
        &self,
        id: &ScriptExecutionId,
        stream: ScriptOutputStream,
        bytes: Vec<u8>,
    ) -> Result<()> {
        self.inner.append_output(id, stream, bytes).await
    }

    async fn finish(
        &self,
        id: &ScriptExecutionId,
        completion: ScriptExecutionCompletion,
    ) -> Result<ScriptExecutionRecord> {
        self.inner.finish(id, completion).await
    }
}
