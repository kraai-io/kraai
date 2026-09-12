use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, eyre};
use kraai_types::{MessageId, RequestUsage};
use tokio::fs;

pub struct RequestUsageStore {
    root: PathBuf,
    hot: tokio::sync::RwLock<BTreeMap<String, BTreeMap<MessageId, RequestUsage>>>,
}

impl RequestUsageStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("usage"),
            hot: Default::default(),
        }
    }

    fn session_dir(&self, session_id: &str) -> Result<PathBuf> {
        MessageId::try_new(session_id).map_err(|error| eyre!(error))?;
        Ok(self.root.join(session_id))
    }

    pub async fn save(&self, session_id: &str, request: &RequestUsage) -> Result<()> {
        MessageId::try_new(request.message_id.as_str()).map_err(|error| eyre!(error))?;
        let path = self
            .session_dir(session_id)?
            .join(format!("{}.json", request.message_id));
        let mut hot = self.hot.write().await;
        crate::atomic_write(&path, &serde_json::to_vec(request)?).await?;
        if let Some(requests) = hot.get_mut(session_id) {
            requests.insert(request.message_id.clone(), request.clone());
        }
        drop(hot);
        Ok(())
    }

    pub async fn delete(&self, session_id: &str) -> Result<()> {
        let path = self.session_dir(session_id)?;
        let mut hot = self.hot.write().await;
        match fs::remove_dir_all(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        hot.remove(session_id);
        drop(hot);
        Ok(())
    }

    pub async fn load(&self, session_id: &str) -> Result<BTreeMap<MessageId, RequestUsage>> {
        if let Some(requests) = self.hot.read().await.get(session_id) {
            return Ok(requests.clone());
        }
        let mut hot = self.hot.write().await;
        if let Some(requests) = hot.get(session_id) {
            return Ok(requests.clone());
        }
        let mut requests = BTreeMap::new();
        let mut entries = match fs::read_dir(self.session_dir(session_id)?).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(requests),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let request: RequestUsage = serde_json::from_slice(&fs::read(entry.path()).await?)?;
                requests.insert(request.message_id.clone(), request);
            }
        }
        hot.insert(session_id.to_string(), requests.clone());
        drop(hot);
        Ok(requests)
    }
}
