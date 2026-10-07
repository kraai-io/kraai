use std::path::Path;
use std::sync::Arc;

use color_eyre::eyre::{Context, Result};

use crate::{
    ContextStateStore, ConversationStore, FileCompactionStore, FileImageStore, FileMessageStore,
    FileScriptExecutionStore, FileSessionStore, MessageStore, RequestUsageStore, SessionStore,
};

#[derive(Clone)]
pub struct Persistence {
    pub(crate) messages: Arc<dyn MessageStore>,
    pub(crate) sessions: Arc<FileSessionStore>,
    pub(crate) executions: Arc<FileScriptExecutionStore>,
    pub(crate) context: Arc<dyn ContextStateStore>,
    pub(crate) usage: Arc<dyn RequestUsageStore>,
    pub(crate) images: Arc<FileImageStore>,
    pub(crate) compactions: FileCompactionStore,
}

impl Persistence {
    pub async fn open(data_dir: &Path) -> Result<Self> {
        Self::open_with_messages(data_dir, Arc::new(FileMessageStore::new(data_dir))).await
    }

    pub async fn open_with_messages(
        data_dir: &Path,
        messages: Arc<dyn MessageStore>,
    ) -> Result<Self> {
        kraai_io::fs::create_dir_all_async(data_dir)
            .await
            .with_context(|| format!("Failed to create data directory: {data_dir:?}"))?;
        let sessions = Arc::new(FileSessionStore::new(data_dir, messages.clone()));
        sessions.load().await?;
        sessions.recover_deletions().await?;
        sessions.cleanup_orphans().await?;
        Ok(Self {
            messages,
            context: sessions.context.clone(),
            usage: sessions.usage.clone(),
            compactions: sessions.compactions(),
            sessions,
            executions: Arc::new(FileScriptExecutionStore::new(data_dir)),
            images: Arc::new(FileImageStore::new(data_dir)),
        })
    }

    pub fn conversations(&self) -> ConversationStore {
        ConversationStore::new(self.messages.clone(), self.sessions.clone())
    }

    pub fn messages(&self) -> &Arc<dyn MessageStore> {
        &self.messages
    }

    pub fn sessions(&self) -> &Arc<FileSessionStore> {
        &self.sessions
    }

    pub fn executions(&self) -> &Arc<FileScriptExecutionStore> {
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

    pub fn compactions(&self) -> &FileCompactionStore {
        &self.compactions
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.sessions.delete(session_id).await
    }
}
