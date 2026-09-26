use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use kraai_persistence::{
    NewScriptExecution, PersistedScriptOutput, ScriptExecutionCompletion, ScriptExecutionRecord,
    ScriptExecutionStore,
};
use kraai_types::{ScriptExecutionId, ScriptOutputStream};
use tokio::sync::Notify;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn deleting_during_script_preparation_cannot_leave_an_orphan_approval() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain(
        "<tool_call>\n# timeout=30sec permissions=workspace-write\n'changed' | save result.txt\n</tool_call>",
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
    {
        let _state_guard = runtime.session_state_barrier.read().await;
        runtime
            .handle_send_message(
                session_id.clone(),
                "change it".into(),
                kraai_types::ModelId::new("mock-model"),
                kraai_types::ProviderId::new("mock"),
            )
            .await?;
    }
    tokio::time::timeout(Duration::from_secs(1), store.entered.notified()).await?;
    harness.handle.delete_session(idle_session.clone()).await?;
    let (response, deleted) = tokio::sync::oneshot::channel();
    let deletion = runtime.handle_command(crate::handle::Command::DeleteSession {
        session_id: session_id.clone(),
        response,
    });
    tokio::pin!(deletion);
    assert!(futures::poll!(&mut deletion).is_pending());
    store.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), &mut deletion).await??;
    let outcome = deleted.await?;
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

    async fn list_for_session(&self, session_id: &str) -> Result<Vec<ScriptExecutionRecord>> {
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
