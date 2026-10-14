use crate::database::{Database, read_record, record_session};
use crate::message_references::{delete_message_record, write_message_record};
use color_eyre::eyre::{Result, ensure, eyre};
use kraai_types::{ConversationItem, MessageId, ModelId, ProviderId, TokenUsage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
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
pub struct SqliteCompactionStore {
    database: Database,
}
impl SqliteCompactionStore {
    pub fn new(data_dir: &Path) -> Self {
        Self::with_database(Database::new(data_dir))
    }
    pub(crate) fn with_database(database: Database) -> Self {
        Self { database }
    }
    pub async fn get(&self, boundary: &MessageId) -> Result<Option<CompactionCheckpoint>> {
        let boundary = boundary.to_string();
        self.database
            .read(move |connection, _| {
                let checkpoint =
                    read_record::<CompactionCheckpoint>(connection, "compaction", &boundary)?;
                if let Some(checkpoint) = &checkpoint {
                    checkpoint.validate()?;
                }
                Ok(checkpoint)
            })
            .await
    }
    pub async fn compatible_checkpoints(
        &self,
        boundaries: impl IntoIterator<Item = &MessageId>,
        provider_id: &ProviderId,
        model_id: &ModelId,
    ) -> Result<HashMap<MessageId, CompactionCheckpoint>> {
        let boundaries = serde_json::to_string(&boundaries.into_iter().collect::<Vec<_>>())?;
        let (provider_id, model_id) = (provider_id.clone(), model_id.clone());
        self.database
            .read(move |connection, _| {
                let mut statement = connection.prepare_cached(
                    "SELECT data FROM records WHERE kind = 'compaction'
                     AND id IN (SELECT value FROM json_each(?1))",
                )?;
                let rows = statement.query_map([boundaries], |row| row.get::<_, String>(0))?;
                let mut checkpoints = HashMap::new();
                for row in rows {
                    let checkpoint: CompactionCheckpoint = serde_json::from_str(&row?)?;
                    checkpoint.validate()?;
                    if checkpoint.compatible_with(&provider_id, &model_id) {
                        checkpoints.insert(checkpoint.covered_through.clone(), checkpoint);
                    }
                }
                Ok(checkpoints)
            })
            .await
    }
    pub async fn save(&self, checkpoint: &CompactionCheckpoint) -> Result<()> {
        checkpoint.validate()?;
        let checkpoint = checkpoint.clone();
        self.database
            .transaction(move |transaction, leases| {
                let boundary = checkpoint.covered_through.as_str();
                let session = record_session(transaction, "message", boundary)?;
                write_message_record(
                    transaction,
                    leases,
                    "compaction",
                    boundary,
                    session.as_ref(),
                    &checkpoint,
                )
            })
            .await
    }
    pub async fn save_with_barrier(
        &self,
        checkpoint: &CompactionCheckpoint,
        barrier: Arc<tokio::sync::RwLock<()>>,
    ) -> Result<()> {
        let guard = barrier.read_owned().await;
        let result = self.save(checkpoint).await;
        drop(guard);
        result
    }
    pub async fn delete(&self, boundary: &MessageId) -> Result<()> {
        let boundary = boundary.to_string();
        self.database
            .transaction(move |transaction, leases| {
                delete_message_record(transaction, leases, "compaction", &boundary)
            })
            .await
    }
}
