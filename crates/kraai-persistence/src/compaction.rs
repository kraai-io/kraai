use kraai_io::fs::atomic_replace_in_async;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{commit::complete_commit, keyed_locks::KeyedLocks};
use color_eyre::eyre::{Context, Result, ensure, eyre};
use kraai_types::{ConversationItem, MessageId, ModelId, ProviderId, TokenUsage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionCheckpoint {
    pub covered_through: MessageId,
    #[serde(default)]
    pub superseded_usage: Vec<MessageId>,
    pub previous_boundary: Option<MessageId>,
    pub replacement: Vec<ConversationItem>,
    pub model_id: ModelId,
    pub provider_id: ProviderId,
    pub prompt_version: u32,
    pub usage: Option<TokenUsage>,
}

impl CompactionCheckpoint {
    pub fn compatible_with(&self, provider_id: &ProviderId, model_id: &ModelId) -> bool {
        !self
            .replacement
            .iter()
            .any(|item| matches!(item, ConversationItem::Compaction { .. }))
            || (&self.provider_id == provider_id && &self.model_id == model_id)
    }

    fn validate(&self) -> Result<()> {
        MessageId::try_new(self.covered_through.as_str()).map_err(|error| eyre!(error))?;
        for message in &self.superseded_usage {
            MessageId::try_new(message.as_str()).map_err(|error| eyre!(error))?;
        }
        if let Some(previous) = &self.previous_boundary {
            MessageId::try_new(previous.as_str()).map_err(|error| eyre!(error))?;
            ensure!(
                previous != &self.covered_through,
                "Compaction checkpoint cannot reference itself"
            );
        }
        ensure!(
            self.prompt_version == 1,
            "Unsupported compaction prompt version"
        );
        ensure!(!self.replacement.is_empty(), "Empty compaction replacement");
        Ok(())
    }
}

#[derive(Clone)]
pub struct FileCompactionStore {
    anchor: PathBuf,
    root: PathBuf,
    locks: Arc<KeyedLocks<MessageId>>,
}

impl FileCompactionStore {
    pub fn new(storage_root: &Path) -> Self {
        Self {
            anchor: storage_root.to_path_buf(),
            root: storage_root.join("compactions"),
            locks: Arc::default(),
        }
    }

    fn path(&self, boundary: &MessageId) -> Result<PathBuf> {
        MessageId::try_new(boundary.as_str()).map_err(|error| eyre!(error))?;
        Ok(self.root.join(format!("{boundary}.json")))
    }

    pub async fn get(&self, boundary: &MessageId) -> Result<Option<CompactionCheckpoint>> {
        let path = self.path(boundary)?;
        let Some(bytes) = kraai_io::fs::read_optional_async(&path)
            .await
            .with_context(|| format!("Failed to read compaction: {path:?}"))?
        else {
            return Ok(None);
        };
        let checkpoint: CompactionCheckpoint = serde_json::from_slice(&bytes)
            .with_context(|| format!("Failed to parse compaction: {path:?}"))?;
        checkpoint.validate()?;
        ensure!(
            &checkpoint.covered_through == boundary,
            "Compaction checkpoint boundary does not match its filename"
        );
        Ok(Some(checkpoint))
    }

    pub async fn save(&self, checkpoint: &CompactionCheckpoint) -> Result<()> {
        self.save_checkpoint(checkpoint, None).await
    }

    pub async fn save_with_barrier(
        &self,
        checkpoint: &CompactionCheckpoint,
        barrier: Arc<tokio::sync::RwLock<()>>,
    ) -> Result<()> {
        self.save_checkpoint(checkpoint, Some(barrier)).await
    }

    async fn save_checkpoint(
        &self,
        checkpoint: &CompactionCheckpoint,
        barrier: Option<Arc<tokio::sync::RwLock<()>>>,
    ) -> Result<()> {
        checkpoint.validate()?;
        let path = self.path(&checkpoint.covered_through)?;
        let bytes = serde_json::to_vec(checkpoint)?;
        let anchor = self.anchor.clone();
        self.commit(&checkpoint.covered_through, barrier, async move {
            Ok(atomic_replace_in_async(&anchor, &path, &bytes)
                .await?
                .into_result()?)
        })
        .await
    }

    pub async fn delete(&self, boundary: &MessageId) -> Result<()> {
        let path = self.path(boundary)?;
        self.commit(boundary, None, async move {
            kraai_io::fs::remove_file_durable_async(&path)
                .await
                .with_context(|| format!("Failed to delete compaction: {path:?}"))?;
            Ok(())
        })
        .await
    }

    async fn commit(
        &self,
        boundary: &MessageId,
        barrier: Option<Arc<tokio::sync::RwLock<()>>>,
        operation: impl Future<Output = Result<()>> + Send + 'static,
    ) -> Result<()> {
        let barrier = match barrier {
            Some(barrier) => Some(barrier.read_owned().await),
            None => None,
        };
        complete_commit(
            self.locks.lock(boundary).await,
            async move {
                let result = operation.await;
                drop(barrier);
                result
            },
            "Compaction commit task failed",
        )
        .await
    }
}

#[cfg(test)]
mod tests;
