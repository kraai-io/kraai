use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, eyre};
use kraai_types::{
    CommandInvocationId, ContextStateEvent, ContextStateEventSource, ContextStateMutation,
    ScriptExecutionId,
};
use serde::{Deserialize, Serialize};
use tokio::fs;
use ulid::Ulid;

use crate::atomic_write;
use crate::commit::complete_commit;
use crate::keyed_locks::KeyedLocks;

#[derive(Debug, Default, Serialize, Deserialize)]
struct ContextStateDocument {
    events: Vec<ContextStateEvent>,
}

#[async_trait::async_trait]
pub trait ContextStateStore: Send + Sync {
    async fn list(&self, session_id: &str) -> Result<Vec<ContextStateEvent>>;

    async fn append_command(
        &self,
        session_id: &str,
        execution_id: &ScriptExecutionId,
        sequence: u64,
        invocation_id: &CommandInvocationId,
        command_id: &str,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent>;

    async fn append_runtime(
        &self,
        session_id: &str,
        component: &str,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent>;

    async fn delete(&self, session_id: &str) -> Result<()>;
}

pub struct FileContextStateStore {
    directory: PathBuf,
    session_locks: KeyedLocks<String>,
}

impl FileContextStateStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            directory: data_dir.join("context-state"),
            session_locks: KeyedLocks::default(),
        }
    }

    fn document_path(&self, session_id: &str) -> Result<PathBuf> {
        if session_id.is_empty()
            || !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(eyre!("Unsafe session id for context state: {session_id:?}"));
        }
        let path = self.directory.join(format!("{session_id}.json"));
        if path.parent() != Some(self.directory.as_path()) {
            return Err(eyre!(
                "Context state path escaped storage directory: {path:?}"
            ));
        }
        Ok(path)
    }

    async fn load_document(&self, session_id: &str) -> Result<ContextStateDocument> {
        let path = self.document_path(session_id)?;
        match fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("Failed to parse context state document: {path:?}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ContextStateDocument::default())
            }
            Err(error) => Err(error)
                .with_context(|| format!("Failed to read context state document: {path:?}")),
        }
    }

    async fn append_event(
        &self,
        session_id: &str,
        source: ContextStateEventSource,
        mutations: Vec<ContextStateMutation>,
    ) -> Result<ContextStateEvent> {
        if mutations.is_empty() {
            return Err(eyre!("Context state events require at least one mutation"));
        }
        let guard = self.session_locks.lock(session_id).await;
        let mut document = self.load_document(session_id).await?;
        if let ContextStateEventSource::Command {
            execution_id,
            sequence,
            invocation_id,
            ..
        } = &source
            && document.events.iter().any(|event| {
                matches!(
                    &event.source,
                    ContextStateEventSource::Command {
                        execution_id: existing_execution,
                        sequence: existing_sequence,
                        invocation_id: existing_invocation,
                        ..
                    } if existing_execution == execution_id
                        && (existing_sequence == sequence || existing_invocation == invocation_id)
                )
            })
        {
            return Err(eyre!(
                "Context state effect {invocation_id} was already persisted for execution {execution_id}"
            ));
        }
        let event = ContextStateEvent {
            id: Ulid::generate().to_string(),
            source,
            mutations,
        };
        document.events.push(event.clone());
        let path = self.document_path(session_id)?;
        let bytes = serde_json::to_vec_pretty(&document)
            .context("Failed to serialize context state document")?;
        complete_commit(
            guard,
            async move { atomic_write(&path, &bytes).await },
            "Context state commit task failed",
        )
        .await
        .map(|()| event)
    }
}

#[async_trait::async_trait]
impl ContextStateStore for FileContextStateStore {
    async fn list(&self, session_id: &str) -> Result<Vec<ContextStateEvent>> {
        let _guard = self.session_locks.lock(session_id).await;
        Ok(self.load_document(session_id).await?.events)
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
        self.append_event(
            session_id,
            ContextStateEventSource::Command {
                execution_id: execution_id.clone(),
                sequence,
                invocation_id: invocation_id.clone(),
                command_id: command_id.to_owned(),
            },
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
        self.append_event(
            session_id,
            ContextStateEventSource::Runtime {
                component: component.to_owned(),
            },
            mutations,
        )
        .await
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        let guard = self.session_locks.lock(session_id).await;
        let path = self.document_path(session_id)?;
        complete_commit(
            guard,
            async move {
                match fs::remove_file(&path).await {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error).with_context(|| {
                        format!("Failed to delete context state document: {path:?}")
                    }),
                }
            },
            "Context state commit task failed",
        )
        .await
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "context state persistence tests use direct fixture assertions"
)]
mod tests {
    use super::*;
    use kraai_types::PinnedFileScope;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kraai-context-state-{name}-{}", Ulid::generate()))
    }

    fn pin(path: &str) -> ContextStateMutation {
        ContextStateMutation::PinFile {
            path: PathBuf::from(path),
            scope: PinnedFileScope::Workspace {
                root: PathBuf::from("/workspace"),
            },
        }
    }

    #[tokio::test]
    async fn cancelled_commit_keeps_later_appends_in_order() {
        let data_dir = test_dir("cancelled-commit");
        let store = FileContextStateStore::new(&data_dir);
        let initial = store
            .append_runtime("session", "initial", vec![pin("/workspace/initial.rs")])
            .await
            .unwrap();
        let cancelled = ContextStateEvent {
            id: String::from("cancelled"),
            source: ContextStateEventSource::Runtime {
                component: String::from("cancelled"),
            },
            mutations: vec![pin("/workspace/cancelled.rs")],
        };
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let caller = {
            let guard = store.session_locks.lock("session").await;
            let mut document = store.load_document("session").await.unwrap();
            document.events.push(cancelled.clone());
            let bytes = serde_json::to_vec_pretty(&document).unwrap();
            let path = store.document_path("session").unwrap();
            tokio::spawn(complete_commit(
                guard,
                async move {
                    let _ = entered_tx.send(());
                    release_rx.await?;
                    atomic_write(&path, &bytes).await
                },
                "Context state commit task failed",
            ))
        };
        entered_rx.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());

        {
            let mut waiting = std::pin::pin!(store.session_locks.lock("session"));
            assert!(
                waiting
                    .as_mut()
                    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                    .is_pending()
            );
        }
        let mut later = std::pin::pin!(store.append_runtime(
            "session",
            "later",
            vec![pin("/workspace/later.rs")],
        ));
        assert!(
            later
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
        store
            .append_runtime("other", "independent", vec![pin("/workspace/other.rs")])
            .await
            .unwrap();
        release_tx.send(()).unwrap();
        let later = later.await.unwrap();
        assert_eq!(
            store.list("session").await.unwrap(),
            vec![initial, cancelled, later]
        );
        fs::remove_dir_all(data_dir).await.unwrap();
    }

    #[tokio::test]
    async fn command_and_runtime_events_survive_recreation_in_order() {
        let data_dir = test_dir("durable");
        let execution_id = ScriptExecutionId::new(Ulid::generate());
        let invocation_id = CommandInvocationId::new(Ulid::generate());
        let store = FileContextStateStore::new(&data_dir);
        store
            .append_command(
                "session",
                &execution_id,
                1,
                &invocation_id,
                "kraai-open-files",
                vec![pin("/workspace/a.rs")],
            )
            .await
            .unwrap();
        store
            .append_runtime(
                "session",
                "pinned-file-refresh",
                vec![ContextStateMutation::UnpinFile {
                    path: PathBuf::from("/workspace/a.rs"),
                    reason: Some(String::from("file no longer exists")),
                }],
            )
            .await
            .unwrap();
        drop(store);

        let reopened = FileContextStateStore::new(&data_dir);
        let events = reopened.list("session").await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events.first().map(|event| &event.source),
            Some(ContextStateEventSource::Command { .. })
        ));
        assert!(matches!(
            events.get(1).map(|event| &event.source),
            Some(ContextStateEventSource::Runtime { .. })
        ));
        let _ = fs::remove_dir_all(data_dir).await;
    }

    #[tokio::test]
    async fn duplicate_command_effects_do_not_mutate_the_log() {
        let data_dir = test_dir("duplicate");
        let execution_id = ScriptExecutionId::new(Ulid::generate());
        let invocation_id = CommandInvocationId::new(Ulid::generate());
        let store = FileContextStateStore::new(&data_dir);
        store
            .append_command(
                "session",
                &execution_id,
                1,
                &invocation_id,
                "kraai-open-files",
                vec![pin("/workspace/a.rs")],
            )
            .await
            .unwrap();
        let error = store
            .append_command(
                "session",
                &execution_id,
                1,
                &invocation_id,
                "kraai-open-files",
                vec![pin("/workspace/b.rs")],
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already persisted"));
        assert_eq!(store.list("session").await.unwrap().len(), 1);
        let _ = fs::remove_dir_all(data_dir).await;
    }
}
