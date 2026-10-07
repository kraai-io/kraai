use super::*;

use kraai_io::fs::{atomic_replace_in_async, remove_file_durable_async};

#[derive(Serialize, Deserialize)]
struct SessionDeletion {
    session_id: String,
    messages: HashSet<MessageId>,
}

impl FileSessionStore {
    pub(crate) async fn recover_deletions(&self) -> Result<()> {
        let mut pending: Vec<_> = self.state.deleting.read().await.iter().cloned().collect();
        pending.sort();
        for id in pending {
            self.delete_session(&id).await?;
        }
        Ok(())
    }

    pub(super) async fn load_deletions(&self) -> Result<()> {
        let mut entries = match fs::read_dir(&self.state.deletions_dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut pending = HashSet::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let deletion: SessionDeletion = serde_json::from_slice(&fs::read(&path).await?)?;
            if path != self.state.deletion_path(&deletion.session_id)? {
                return Err(eyre!(
                    "Session deletion marker does not match its filename: {path:?}"
                ));
            }
            pending.insert(deletion.session_id);
        }
        *self.state.deleting.write().await = pending;
        Ok(())
    }

    pub(super) async fn delete_session(&self, id: &str) -> Result<()> {
        let marker = self.state.deletion_path(id)?;
        let guard = self.write_guard.clone().lock_owned().await;
        let state = self.state.clone();
        let context = self.context.clone();
        let usage = self.usage.clone();
        let id = id.to_string();
        complete_commit(
            guard,
            async move {
                let deletion = state.prepare_deletion(&id, &marker).await?;
                let mut remaining = state.sessions.read().await.clone();
                remaining.remove(&id);
                let outcome =
                    SessionState::persist_sessions(&remaining, &state.sessions_path).await?;
                state.publish_sessions(remaining, outcome).await?;
                state
                    .gc_orphaned_messages(deletion.messages, state.message_store.as_ref())
                    .await?;
                context.delete(&id).await?;
                usage.delete(&id).await?;
                remove_file_durable_async(&marker).await?;
                state.deleting.write().await.remove(&id);
                Ok(())
            },
            "Session deletion task failed",
        )
        .await
    }
}

impl SessionState {
    fn deletion_path(&self, id: &str) -> Result<PathBuf> {
        MessageId::try_new(id).map_err(|error| eyre!(error))?;
        Ok(self.deletions_dir.join(format!("{id}.json")))
    }

    async fn prepare_deletion(&self, id: &str, path: &Path) -> Result<SessionDeletion> {
        let deletion = match kraai_io::fs::read_optional_async(path).await? {
            Some(bytes) => {
                let deletion: SessionDeletion = serde_json::from_slice(&bytes)?;
                if deletion.session_id != id {
                    return Err(eyre!(
                        "Session deletion marker has an unexpected session ID"
                    ));
                }
                deletion
            }
            None => {
                let tip = self
                    .sessions
                    .read()
                    .await
                    .get(id)
                    .and_then(|session| session.tip_id.clone());
                let messages = match tip {
                    Some(tip) => self.collect_tree_messages(&tip).await?,
                    None => HashSet::new(),
                };
                SessionDeletion {
                    session_id: id.to_string(),
                    messages,
                }
            }
        };
        let anchor = self
            .deletions_dir
            .parent()
            .ok_or_else(|| eyre!("Session deletion storage has no data directory"))?;
        let outcome =
            atomic_replace_in_async(anchor, path, &serde_json::to_vec(&deletion)?).await?;
        self.deleting.write().await.insert(id.to_string());
        outcome.into_result()?;
        Ok(deletion)
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic_in_result_fn,
    reason = "deletion recovery tests assert filesystem failure outcomes"
)]
mod tests;
