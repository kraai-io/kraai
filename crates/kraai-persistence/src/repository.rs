use crate::database::Database;
use crate::{
    ContextStateStore, ConversationStore, FileImageStore, MessageStore, RequestUsageStore,
    SessionStore, SqliteCompactionStore, SqliteContextStateStore, SqliteMessageStore,
    SqliteRequestUsageStore, SqliteScriptExecutionStore, SqliteSessionStore, TURN_LEASE_DURATION,
};
use color_eyre::eyre::{Result, ensure};
use kraai_types::SessionId;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct Persistence {
    pub(crate) messages: Arc<dyn MessageStore>,
    pub(crate) sessions: Arc<SqliteSessionStore>,
    pub(crate) executions: Arc<SqliteScriptExecutionStore>,
    pub(crate) context: Arc<dyn ContextStateStore>,
    pub(crate) usage: Arc<dyn RequestUsageStore>,
    pub(crate) images: Arc<FileImageStore>,
    pub(crate) compactions: SqliteCompactionStore,
}

impl Persistence {
    pub async fn open(data_dir: &Path) -> Result<Self> {
        Self::open_with_turn_lease(data_dir, TURN_LEASE_DURATION).await
    }
    pub async fn open_with_turn_lease(
        data_dir: &Path,
        turn_lease_duration: Duration,
    ) -> Result<Self> {
        ensure!(
            !turn_lease_duration.is_zero(),
            "Lease duration must be positive"
        );
        let database = Database::new(data_dir);
        let messages = Arc::new(SqliteMessageStore::with_database(database.clone()));
        Self::with_database(data_dir, database, messages, turn_lease_duration).await
    }
    pub async fn open_with_messages(
        data_dir: &Path,
        messages: Arc<dyn MessageStore>,
    ) -> Result<Self> {
        let database = messages
            .sqlite_database()
            .unwrap_or_else(|| Database::new(data_dir));
        Self::with_database(data_dir, database, messages, TURN_LEASE_DURATION).await
    }
    async fn with_database(
        data_dir: &Path,
        database: Database,
        messages: Arc<dyn MessageStore>,
        turn_lease_duration: Duration,
    ) -> Result<Self> {
        let sessions = Arc::new(SqliteSessionStore::with_database(
            database.clone(),
            turn_lease_duration,
        ));
        sessions.load().await?;
        Ok(Self {
            messages,
            context: Arc::new(SqliteContextStateStore::with_database(database.clone())),
            usage: Arc::new(SqliteRequestUsageStore::with_database(database.clone())),
            compactions: SqliteCompactionStore::with_database(database.clone()),
            sessions,
            executions: Arc::new(SqliteScriptExecutionStore::with_database(database)),
            images: Arc::new(FileImageStore::new(data_dir)),
        })
    }
    pub fn conversations(&self) -> ConversationStore {
        ConversationStore::new(self.messages.clone(), self.sessions.clone())
    }
    pub fn messages(&self) -> &Arc<dyn MessageStore> {
        &self.messages
    }
    pub fn sessions(&self) -> &Arc<SqliteSessionStore> {
        &self.sessions
    }
    pub fn executions(&self) -> &Arc<SqliteScriptExecutionStore> {
        &self.executions
    }
    pub fn context(&self) -> &Arc<dyn ContextStateStore> {
        &self.context
    }
    pub fn usage(&self) -> &Arc<dyn RequestUsageStore> {
        &self.usage
    }
    pub fn images(&self) -> &Arc<FileImageStore> {
        &self.images
    }
    pub fn compactions(&self) -> &SqliteCompactionStore {
        &self.compactions
    }
    pub async fn delete_session(&self, session_id: &SessionId) -> Result<()> {
        self.sessions.delete(session_id).await
    }
}
