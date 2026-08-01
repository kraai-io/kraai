use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use color_eyre::eyre::{Context, Result, eyre};
use kraai_types::{CommandInvocationId, ScriptExecutionId};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::{Mutex, RwLock};
use ulid::Ulid;

use crate::atomic_write;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum PinnedFileScope {
    Workspace { root: PathBuf },
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ContextStateMutation {
    PinFile {
        path: PathBuf,
        scope: PinnedFileScope,
    },
    UnpinFile {
        path: PathBuf,
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ContextStateEventSource {
    Command {
        execution_id: ScriptExecutionId,
        sequence: u64,
        invocation_id: CommandInvocationId,
        command_id: String,
    },
    Runtime {
        component: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextStateEvent {
    pub id: String,
    pub source: ContextStateEventSource,
    pub mutations: Vec<ContextStateMutation>,
}

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
    session_locks: RwLock<HashMap<String, Arc<Mutex<()>>>>,
}

impl FileContextStateStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            directory: data_dir.join("context-state"),
            session_locks: RwLock::new(HashMap::new()),
        }
    }

    async fn session_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        if let Some(lock) = self.session_locks.read().await.get(session_id).cloned() {
            return lock;
        }
        self.session_locks
            .write()
            .await
            .entry(session_id.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
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
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
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
        atomic_write(&path, &bytes).await?;
        Ok(event)
    }
}

#[async_trait::async_trait]
impl ContextStateStore for FileContextStateStore {
    async fn list(&self, session_id: &str) -> Result<Vec<ContextStateEvent>> {
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
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
        let lock = self.session_lock(session_id).await;
        let _guard = lock.lock().await;
        let path = self.document_path(session_id)?;
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("Failed to delete context state document: {path:?}")),
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "context state persistence tests use direct fixture assertions"
)]
mod tests {
    use super::*;

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
