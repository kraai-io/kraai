use kraai_io::lock::{FileLock, open_private_lock_file};
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
        match kraai_io::fs::read_optional(&self.path).map_err(error)? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(error),
            None => Ok(Document::default()),
        }
    }

    fn write(&self, document: &Document) -> Result<(), AuthError> {
        let bytes = serde_json::to_vec(document).map_err(error)?;
        kraai_io::fs::atomic_replace_private(&self.path, &bytes)
            .and_then(kraai_io::fs::AtomicWriteOutcome::into_result)
            .map_err(error)
    }

    pub(super) async fn lock(&self) -> Result<FileLock, AuthError> {
        let path = self.path.clone();
        let file = tokio::task::spawn_blocking(move || {
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::other("Missing credential directory"))?;
            kraai_io::fs::create_private_dir_all(parent)?;
            open_private_lock_file(&path.with_extension("lock"))
        })
        .await
        .map_err(error)?
        .map_err(error)?;
        FileLock::acquire_until(
            file,
            Some(tokio::time::Instant::now() + Duration::from_secs(10)),
            Duration::from_millis(20),
        )
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::TimedOut {
                error("Timed out waiting for MCP credential lock")
            } else {
                error(err)
            }
        })
    }

    pub(super) async fn reset(&self) -> Result<Self, AuthError> {
        self.commit_locked(|store| {
            let generation = store
                .read()?
                .generation
                .checked_add(1)
                .ok_or_else(|| error("Credential generation exhausted"))?;
            store.write(&Document {
                generation,
                ..Document::default()
            })?;
            Ok(Self {
                path: store.path,
                generation,
            })
        })
        .await
    }

    async fn commit_locked<T: Send + 'static>(
        &self,
        commit: impl FnOnce(Self) -> Result<T, AuthError> + Send + 'static,
    ) -> Result<T, AuthError> {
        let guard = self.lock().await?;
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let result = commit(store);
            drop(guard);
            result
        })
        .await
        .map_err(error)?
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
        self.commit_locked(move |store| {
            let mut document = store.read()?;
            if document.generation != store.generation {
                return Err(AuthError::AuthorizationRequired);
            }
            document.registration_secret = secret;
            store.write(&document)
        })
        .await
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

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "credential commit tests assert cancellation and lock ownership"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_commit_keeps_lock_until_blocking_write_finishes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth/credentials.json");
        let store = FileStore::current(path.clone())
            .unwrap()
            .reset()
            .await
            .unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let committing = store.clone();
        let caller = tokio::spawn(async move {
            committing
                .commit_locked(move |store| {
                    let _ = entered_tx.send(());
                    release_rx.blocking_recv().map_err(error)?;
                    store.write(&Document {
                        generation: store.generation,
                        registration_secret: Some(String::from("committed")),
                        ..Document::default()
                    })
                })
                .await
        });
        entered_rx.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let contender = open_private_lock_file(&path.with_extension("lock")).unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        tokio::time::sleep(Duration::from_millis(1)).await;
        release_tx.send(()).unwrap();
        let guard = store.lock().await.unwrap();
        assert_eq!(
            store.registration_secret().unwrap().as_deref(),
            Some("committed")
        );
        drop(guard);
        drop(contender);
    }
}
