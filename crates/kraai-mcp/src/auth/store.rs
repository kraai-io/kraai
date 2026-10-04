use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use rmcp::transport::auth::{
    AuthError, CredentialRefreshGuard, CredentialStore, StoredCredentials,
};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub(super) struct FileStore {
    path: PathBuf,
    generation: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Document {
    generation: u64,
    credentials: Option<StoredCredentials>,
    registration_secret: Option<String>,
}

fn error(error: impl std::fmt::Display) -> AuthError {
    AuthError::CredentialStoreError(error.to_string())
}

impl FileStore {
    pub(super) fn current(path: PathBuf) -> Result<Self, AuthError> {
        let mut store = Self {
            path,
            generation: 0,
        };
        store.generation = store.read()?.generation;
        Ok(store)
    }

    fn read(&self) -> Result<Document, AuthError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(error),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Document::default()),
            Err(err) => Err(error(err)),
        }
    }

    fn write(&self, document: &Document) -> Result<(), AuthError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("Missing credential directory"))?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        serde_json::to_writer(&mut file, document).map_err(error)?;
        file.flush().map_err(error)?;
        file.as_file().sync_all().map_err(error)?;
        file.persist(&self.path).map_err(error)?;
        Ok(())
    }

    pub(super) async fn lock(&self) -> Result<File, AuthError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("Missing credential directory"))?;
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(parent).map_err(error)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(self.path.with_extension("lock"))
            .map_err(error)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(error("Timed out waiting for MCP credential lock"));
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(err) => return Err(error(err)),
            }
        }
    }

    pub(super) async fn reset(&self) -> Result<Self, AuthError> {
        let _guard = self.lock().await?;
        let generation = self
            .read()?
            .generation
            .checked_add(1)
            .ok_or_else(|| error("Credential generation exhausted"))?;
        self.write(&Document {
            generation,
            ..Document::default()
        })?;
        Ok(Self {
            path: self.path.clone(),
            generation,
        })
    }

    pub(super) fn registration_secret(&self) -> Result<Option<String>, AuthError> {
        let document = self.read()?;
        if document.generation != self.generation {
            return Err(AuthError::AuthorizationRequired);
        }
        Ok(document.registration_secret)
    }

    pub(super) async fn save_registration_secret(
        &self,
        secret: Option<String>,
    ) -> Result<(), AuthError> {
        let _guard = self.lock().await?;
        let mut document = self.read()?;
        if document.generation != self.generation {
            return Err(AuthError::AuthorizationRequired);
        }
        document.registration_secret = secret;
        self.write(&document)
    }
}

#[async_trait::async_trait]
impl CredentialStore for FileStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let document = self.read()?;
        if document.generation != self.generation {
            return Err(AuthError::AuthorizationRequired);
        }
        Ok(document.credentials)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let mut document = self.read()?;
        if document.generation != self.generation {
            return Err(AuthError::AuthorizationRequired);
        }
        document.credentials = Some(credentials);
        self.write(&document)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        if self.read()?.generation != self.generation {
            return Err(AuthError::AuthorizationRequired);
        }
        self.write(&Document {
            generation: self.generation,
            ..Document::default()
        })
    }

    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        Ok(Some(CredentialRefreshGuard::new(self.lock().await?)))
    }
}
