use super::*;

use color_eyre::Result;
use kraai_persistence::{ContextStateDocument, FileContextSnapshot, FileContextStateStore};
use kraai_types::ContextStateEvent;

struct PausedContextStore {
    inner: FileContextStateStore,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl ContextStateStore for PausedContextStore {
    async fn load(&self, session_id: &str) -> Result<ContextStateDocument> {
        self.inner.load(session_id).await
    }

    async fn append_command(
        &self,
        session_id: &str,
        execution_id: &ScriptExecutionId,
        sequence: u64,
        invocation_id: &CommandInvocationId,
        command_id: &str,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner
            .append_command(
                session_id,
                execution_id,
                sequence,
                invocation_id,
                command_id,
                mutations,
            )
            .await
    }

    async fn append_runtime(
        &self,
        session_id: &str,
        component: &str,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent> {
        self.inner
            .append_runtime(session_id, component, mutations)
            .await
    }

    async fn save_snapshots(
        &self,
        session_id: &str,
        through_event: Option<&str>,
        snapshots: Vec<FileContextSnapshot>,
        removals: Vec<ContextStateMutation>,
    ) -> Result<()> {
        self.inner
            .save_snapshots(session_id, through_event, snapshots, removals)
            .await
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        self.inner.delete(session_id).await
    }
}

#[tokio::test]
async fn cancelled_unpolled_effect_commit_keeps_session_and_shutdown_ownership() {
    let directory = std::env::temp_dir().join(format!("kraai-state-effects-{}", Ulid::generate()));
    tokio::fs::create_dir_all(&directory).await.unwrap();
    let store = Arc::new(PausedContextStore {
        inner: FileContextStateStore::new(&directory),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let tasks = super::super::super::stream_tasks::StreamTasks::default();
    let barrier = Arc::new(tokio::sync::RwLock::new(()));
    let handler = DurableStateEffects {
        execution_id: ScriptExecutionId::new(Ulid::generate()),
        session_id: "session".into(),
        workspace_root: PathBuf::from("/workspace"),
        effective_capabilities: SandboxCapabilities::workspace_read(),
        store: store.clone(),
        barrier: barrier.clone(),
        session_task: Arc::new(tasks.session_token("session")),
    };
    let request = open_request("/workspace/source.rs");
    {
        let mut applying = handler.apply(&request);
        assert!(futures::poll!(&mut applying).is_pending());
    }
    drop(handler);
    let entered = store.entered.notified();
    tokio::pin!(entered);
    assert!(futures::poll!(&mut entered).is_pending());
    let drain = tasks.wait_session("session");
    tokio::pin!(drain);
    assert!(futures::poll!(&mut drain).is_pending());
    tasks.close();
    let all = tasks.wait();
    tokio::pin!(all);
    assert!(futures::poll!(&mut all).is_pending());
    assert!(barrier.try_write().is_err());
    entered.await;
    assert!(futures::poll!(&mut drain).is_pending());
    assert!(futures::poll!(&mut all).is_pending());
    store.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), drain)
        .await
        .unwrap();
    all.await;
    drop(barrier.write().await);
    let reopened = FileContextStateStore::new(&directory);
    let events = reopened.list("session").await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events.first().unwrap().mutations,
        vec![ContextStateMutation::PinFile {
            path: PathBuf::from("/workspace/source.rs"),
            scope: PinnedFileScope::Workspace {
                root: PathBuf::from("/workspace")
            },
        }]
    );
    reopened.delete("session").await.unwrap();
    assert!(reopened.list("session").await.unwrap().is_empty());
    tokio::fs::remove_dir_all(directory).await.unwrap();
}
