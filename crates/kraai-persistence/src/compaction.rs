use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, ensure, eyre};
use kraai_types::{ConversationItem, MessageId, ModelId, ProviderId, TokenUsage};
use serde::{Deserialize, Serialize};
use tokio::fs;

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
    root: PathBuf,
}

impl FileCompactionStore {
    pub fn new(storage_root: &Path) -> Self {
        Self {
            root: storage_root.join("compactions"),
        }
    }

    fn path(&self, boundary: &MessageId) -> Result<PathBuf> {
        MessageId::try_new(boundary.as_str()).map_err(|error| eyre!(error))?;
        Ok(self.root.join(format!("{boundary}.json")))
    }

    pub async fn get(&self, boundary: &MessageId) -> Result<Option<CompactionCheckpoint>> {
        let path = self.path(boundary)?;
        let bytes = match fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to read compaction: {path:?}"));
            }
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
        checkpoint.validate()?;
        let path = self.path(&checkpoint.covered_through)?;
        crate::atomic_write(&path, &serde_json::to_vec(checkpoint)?).await
    }

    pub async fn delete(&self, boundary: &MessageId) -> Result<()> {
        let path = self.path(boundary)?;
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("Failed to delete compaction: {path:?}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!("kraai-compaction-{}", ulid::Ulid::generate()))
    }

    fn checkpoint() -> CompactionCheckpoint {
        CompactionCheckpoint {
            covered_through: MessageId::new("boundary"),
            superseded_usage: vec![MessageId::new("latest")],
            previous_boundary: Some(MessageId::new("previous")),
            replacement: vec![ConversationItem::User {
                text: "User requested a parser; the parser is implemented.".into(),
            }],
            model_id: ModelId::new("model"),
            provider_id: ProviderId::new("provider"),
            prompt_version: 1,
            usage: Some(TokenUsage {
                input_tokens: 120,
                output_tokens: 15,
                ..TokenUsage::default()
            }),
        }
    }

    #[tokio::test]
    async fn checkpoint_survives_restart_without_changing_history() -> Result<()> {
        let directory = directory();
        let store = FileCompactionStore::new(&directory);
        let checkpoint = checkpoint();
        ensure!(store.get(&checkpoint.covered_through).await?.is_none());
        let history = directory.join("messages").join("boundary.json");
        crate::atomic_write(&history, b"original history").await?;
        store.save(&checkpoint).await?;
        let reopened = FileCompactionStore::new(&directory);
        ensure!(reopened.get(&checkpoint.covered_through).await?.as_ref() == Some(&checkpoint));
        ensure!(fs::read(&history).await? == b"original history");
        fs::remove_dir_all(directory).await?;
        Ok(())
    }

    #[tokio::test]
    async fn rejects_unsafe_ids_and_invalid_checkpoints_without_replacing_saved_state() -> Result<()>
    {
        let directory = directory();
        let store = FileCompactionStore::new(&directory);
        let original = checkpoint();
        store.save(&original).await?;
        for raw in ["../escape", "/tmp/escape", r"..\escape", "C:escape", ""] {
            let id = MessageId(Arc::from(raw));
            ensure!(store.get(&id).await.is_err());
            let mut invalid = original.clone();
            invalid.covered_through = id.clone();
            ensure!(store.save(&invalid).await.is_err());
            invalid.covered_through = original.covered_through.clone();
            invalid.previous_boundary = Some(id);
            ensure!(store.save(&invalid).await.is_err());
        }
        let mut invalid = original.clone();
        invalid.replacement.clear();
        ensure!(store.save(&invalid).await.is_err());
        invalid = original.clone();
        invalid.prompt_version = 2;
        ensure!(store.save(&invalid).await.is_err());
        invalid = original.clone();
        invalid.previous_boundary = Some(invalid.covered_through.clone());
        ensure!(store.save(&invalid).await.is_err());
        ensure!(store.get(&original.covered_through).await?.as_ref() == Some(&original));
        fs::remove_dir_all(directory).await?;
        Ok(())
    }

    #[tokio::test]
    async fn rejects_corrupt_or_mismatched_files() -> Result<()> {
        let directory = directory();
        let store = FileCompactionStore::new(&directory);
        let checkpoint = checkpoint();
        store.save(&checkpoint).await?;
        let other = MessageId::new("other");
        crate::atomic_write(&store.path(&other)?, &serde_json::to_vec(&checkpoint)?).await?;
        ensure!(store.get(&other).await.is_err());
        for bytes in [b"{".as_slice(), b"null"] {
            fs::write(store.path(&other)?, bytes).await?;
            ensure!(store.get(&other).await.is_err());
        }
        let mut invalid = checkpoint.clone();
        invalid.replacement.clear();
        fs::write(
            store.path(&checkpoint.covered_through)?,
            serde_json::to_vec(&invalid)?,
        )
        .await?;
        ensure!(store.get(&checkpoint.covered_through).await.is_err());
        invalid = checkpoint.clone();
        invalid.prompt_version = 0;
        fs::write(
            store.path(&checkpoint.covered_through)?,
            serde_json::to_vec(&invalid)?,
        )
        .await?;
        ensure!(store.get(&checkpoint.covered_through).await.is_err());
        fs::remove_dir_all(directory).await?;
        Ok(())
    }

    #[tokio::test]
    async fn failed_write_keeps_previous_checkpoint_readable() -> Result<()> {
        let directory = directory();
        let store = FileCompactionStore::new(&directory);
        let original = checkpoint();
        store.save(&original).await?;
        let mut next = original.clone();
        next.covered_through = MessageId::new("next");
        next.previous_boundary = Some(original.covered_through.clone());
        fs::create_dir(store.path(&next.covered_through)?).await?;
        ensure!(store.save(&next).await.is_err());
        ensure!(store.get(&original.covered_through).await?.as_ref() == Some(&original));
        let mut entries = fs::read_dir(&store.root).await?;
        while let Some(entry) = entries.next_entry().await? {
            ensure!(
                entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "tmp")
            );
        }
        fs::remove_dir_all(directory).await?;
        Ok(())
    }

    #[tokio::test]
    async fn failed_checkpoint_deletion_preserves_source_message() -> Result<()> {
        use crate::{FileMessageStore, MessageStore};
        use kraai_types::{ConversationItem, Message, MessageStatus};

        let directory = directory();
        let store = FileCompactionStore::new(&directory);
        let messages = FileMessageStore::new(&directory);
        let checkpoint = checkpoint();
        let message = Message {
            id: checkpoint.covered_through.clone(),
            parent_id: None,
            content: ConversationItem::User {
                text: String::from("hello"),
            },
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        };
        messages.save(&message).await?;
        let path = store.path(&message.id)?;
        fs::create_dir_all(&path).await?;
        ensure!(messages.delete(&message.id).await.is_err());
        let retained = messages
            .get(&message.id)
            .await?
            .ok_or_else(|| eyre!("Source message was removed after checkpoint deletion failed"))?;
        ensure!(retained.id == message.id);
        ensure!(retained.content == message.content);
        ensure!(messages.exists(&message.id).await?);
        fs::remove_dir(&path).await?;
        store.save(&checkpoint).await?;
        messages.delete(&message.id).await?;
        ensure!(messages.get(&message.id).await?.is_none());
        ensure!(store.get(&message.id).await?.is_none());
        messages.delete(&message.id).await?;
        fs::remove_dir_all(directory).await?;
        Ok(())
    }
}
