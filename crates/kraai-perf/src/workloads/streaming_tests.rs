use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use kraai_persistence::{
    ConversationSnapshot, MessageStore, SessionMeta, SessionStore, SqliteDatabase,
    SqliteMessageStore,
};
use kraai_types::Message;
use tokio::sync::Mutex;

use super::*;

struct ObservedSnapshots {
    inner: SqliteMessageStore,
    expected: String,
    counts: Mutex<BTreeMap<MessageId, usize>>,
}

#[async_trait::async_trait]
impl MessageStore for ObservedSnapshots {
    fn sqlite_database(&self) -> Option<SqliteDatabase> {
        self.inner.sqlite_database()
    }

    async fn read_conversation(&self, session: &str) -> Result<Option<ConversationSnapshot>> {
        self.inner.read_conversation(session).await
    }

    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.inner.get(id).await
    }

    async fn save(&self, message: &Message) -> Result<()> {
        self.inner.save(message).await?;
        if !matches!(message.status, MessageStatus::Streaming { .. })
            || message.content.display_text().is_empty()
        {
            return Ok(());
        }
        let saved = self
            .inner
            .get(&message.id)
            .await?
            .ok_or_eyre("Published snapshot was not persisted")?;
        let mut counts = self.counts.lock().await;
        let count = counts.get(&message.id).copied().unwrap_or_default() + 1;
        let expected = self
            .expected
            .get(..count * SNAPSHOT_INTERVAL * provider::CHUNK_BYTES)
            .ok_or_eyre("Workload published too many snapshots")?;
        ensure!(
            saved.content.display_text() == expected,
            "Published snapshot has the wrong partial response"
        );
        ensure!(
            matches!(saved.status, MessageStatus::Streaming { .. }),
            "Snapshot was checked after response completion"
        );
        counts.insert(message.id.clone(), count);
        drop(counts);
        Ok(())
    }

    async fn save_linked(
        &self,
        message: &Message,
        session: &SessionMeta,
        expected_tip: Option<&MessageId>,
        sessions: Arc<dyn SessionStore>,
    ) -> Result<bool> {
        self.inner
            .save_linked(message, session, expected_tip, sessions)
            .await
    }

    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.inner.delete(id).await
    }

    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }

    async fn list_ids(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_ids().await
    }
}

#[tokio::test]
async fn workload_persists_every_partial_snapshot_before_completion() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let context = Context {
        directory: directory.path().to_path_buf(),
        profiling: false,
    };
    agent::prepare(&context.directory)?;
    let storage = context.directory.join("storage");
    let observed = Arc::new(ObservedSnapshots {
        inner: SqliteMessageStore::new(&storage),
        expected: expected_reply(),
        counts: Mutex::default(),
    });
    let persistence = Persistence::open_with_messages(&storage, observed.clone()).await?;
    let manager = AgentManager::new(
        provider::manager(),
        context.directory.join("workspace"),
        persistence.clone(),
        storage,
    );
    let mut fixture = Fixture::new(manager, persistence).await?;
    let replies = Case::run(&context, &mut fixture).await?;
    let counts = observed.counts.lock().await.clone();
    ensure!(
        counts.len() == TURNS,
        "Workload omitted snapshot publications"
    );
    for reply in &replies {
        ensure!(
            counts.get(&reply.id) == Some(&(provider::CHUNKS / SNAPSHOT_INTERVAL)),
            "Workload skipped or repeated a partial snapshot"
        );
    }
    drop(observed);
    Case::verify(&context, fixture, replies).await
}
