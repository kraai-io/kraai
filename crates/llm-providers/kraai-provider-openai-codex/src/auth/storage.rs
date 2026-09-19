use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use rand::Rng;
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
    if !path.exists() {
        return Ok(None);
    }

    let file = std::fs::read(path)?;
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

pub(super) fn persist_auth_file(path: &Path, auth: &StoredAuth) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = serde_json::to_vec_pretty(&StoredAuthFile {
        auth_mode: "chatgpt".to_string(),
        tokens: auth.tokens.clone(),
        last_refresh: auth.last_refresh_unix,
        generation: auth.generation.clone(),
    })
    .map_err(io::Error::other)?;
    let temp_path = temp_auth_write_path(path);
    let mut temp_file = create_auth_temp_file(&temp_path)?;
    let write_result: io::Result<()> = (|| {
        temp_file.write_all(&payload)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temp_file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
    drop(temp_file);
    let result = write_result.and_then(|()| std::fs::rename(&temp_path, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

pub(super) fn create_auth_temp_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn temp_auth_write_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "auth.json".to_string());
    let mut random_bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut random_bytes);
    let suffix = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes);
    path.with_file_name(format!(".{file_name}.{suffix}.tmp"))
}

pub(super) fn delete_auth_file(path: &Path) -> io::Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub(super) struct AuthFileLock {
    _file: File,
}

impl AuthFileLock {
    fn acquire(auth_path: &Path) -> io::Result<Self> {
        if let Some(parent) = auth_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = auth_path.with_extension("json.refresh.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

pub(super) async fn acquire_auth_file_lock(auth_path: PathBuf) -> io::Result<AuthFileLock> {
    tokio::task::spawn_blocking(move || AuthFileLock::acquire(&auth_path))
        .await
        .map_err(io::Error::other)?
}
