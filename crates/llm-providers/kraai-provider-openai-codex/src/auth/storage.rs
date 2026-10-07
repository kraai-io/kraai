use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kraai_io::fs::{AtomicWriteOutcome, DirectoryBootstrap};
use kraai_io::lock::{FileLock, open_private_lock_file};
use serde::{Deserialize, Serialize};

use super::token::{StoredAuth, StoredTokens, parse_id_token_claims, token_generation};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAuthFile {
    auth_mode: String,
    tokens: StoredTokens,
    last_refresh: u64,
    #[serde(default)]
    generation: String,
}

pub(super) fn load_auth_file(path: &Path) -> io::Result<Option<StoredAuth>> {
    let Some(file) = kraai_io::fs::read_optional(path)? else {
        return Ok(None);
    };
    let stored = serde_json::from_slice::<StoredAuthFile>(&file).map_err(io::Error::other)?;
    let claims = parse_id_token_claims(&stored.tokens.id_token)?;
    let generation = if stored.generation.is_empty() {
        token_generation(&stored.tokens.refresh_token)
    } else {
        stored.generation
    };
    Ok(Some(StoredAuth {
        tokens: stored.tokens,
        claims,
        last_refresh_unix: stored.last_refresh,
        generation,
    }))
}

pub(super) fn persist_auth_file(path: &Path, auth: &StoredAuth) -> io::Result<AtomicWriteOutcome> {
    let payload = serde_json::to_vec_pretty(&StoredAuthFile {
        auth_mode: "chatgpt".to_string(),
        tokens: auth.tokens.clone(),
        last_refresh: auth.last_refresh_unix,
        generation: auth.generation.clone(),
    })
    .map_err(io::Error::other)?;
    kraai_io::fs::atomic_replace_private(path, &payload)
}

pub(super) fn delete_auth_file(path: &Path) -> io::Result<()> {
    kraai_io::fs::remove_file_durable(path).map(|_removed| ())
}

pub(super) async fn acquire_auth_file_lock(
    auth_path: PathBuf,
    directory: Arc<DirectoryBootstrap>,
) -> io::Result<Arc<FileLock>> {
    let file = tokio::task::spawn_blocking(move || {
        directory.create_private()?;
        open_private_lock_file(&auth_path.with_extension("json.refresh.lock"))
    })
    .await
    .map_err(io::Error::other)??;
    FileLock::acquire_until(file, None, std::time::Duration::from_millis(20))
        .await
        .map(Arc::new)
}
