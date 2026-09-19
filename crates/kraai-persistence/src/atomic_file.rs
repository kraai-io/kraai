#[cfg(not(windows))]
use std::future::Future;
use std::path::Path;
#[cfg(any(not(windows), test))]
use std::path::PathBuf;

use color_eyre::eyre::{Context, Result, eyre};
#[cfg(not(windows))]
use tokio::fs;
#[cfg(not(windows))]
use tokio::io::AsyncWriteExt;
#[cfg(any(not(windows), test))]
use ulid::Ulid;

/// Atomically replace `path` and make the acknowledged write crash-durable.
///
/// Unix persists both the file contents and containing directory entry. Other
/// platforms persist the file contents before replacement but may not expose a
/// portable directory-sync operation.
pub async fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    atomic_write_with_outcome(path, content)
        .await?
        .into_result()
}

#[derive(Debug)]
pub(crate) enum AtomicWriteOutcome {
    Durable,
    #[cfg(not(windows))]
    ReplacedButNotSynced(color_eyre::Report),
}

impl AtomicWriteOutcome {
    pub(crate) fn into_result(self) -> Result<()> {
        match self {
            Self::Durable => Ok(()),
            #[cfg(not(windows))]
            Self::ReplacedButNotSynced(error) => Err(error),
        }
    }
}

#[cfg(not(windows))]
pub(crate) async fn atomic_write_with_outcome(
    path: &Path,
    content: &[u8],
) -> Result<AtomicWriteOutcome> {
    atomic_write_with_parent_sync(path, content, sync_parent_directory).await
}

#[cfg(not(windows))]
async fn atomic_write_with_parent_sync<'a, F, Fut>(
    path: &'a Path,
    content: &[u8],
    sync_parent: F,
) -> Result<AtomicWriteOutcome>
where
    F: FnOnce(&'a Path) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("Cannot atomically write path without a parent: {path:?}"))?;
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("Failed to create directory: {parent:?}"))?;

    let temp_path = temp_write_path(path);
    let mut temp_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await
        .with_context(|| format!("Failed to create temp file: {temp_path:?}"))?;
    let write_result: Result<()> = async {
        temp_file
            .write_all(content)
            .await
            .with_context(|| format!("Failed to write temp file: {temp_path:?}"))?;
        temp_file
            .flush()
            .await
            .with_context(|| format!("Failed to flush temp file: {temp_path:?}"))?;
        temp_file
            .sync_all()
            .await
            .with_context(|| format!("Failed to sync temp file: {temp_path:?}"))?;
        drop(temp_file);

        fs::rename(&temp_path, path)
            .await
            .with_context(|| format!("Failed to rename temp file to: {path:?}"))?;
        Ok(())
    }
    .await;

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path).await;
    }
    write_result?;
    Ok(match sync_parent(parent).await {
        Ok(()) => AtomicWriteOutcome::Durable,
        Err(error) => AtomicWriteOutcome::ReplacedButNotSynced(error),
    })
}

#[cfg(windows)]
pub(crate) async fn atomic_write_with_outcome(
    path: &Path,
    content: &[u8],
) -> Result<AtomicWriteOutcome> {
    let path = path.to_path_buf();
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || atomic_write_sync(&path, &content))
        .await
        .map_err(|error| eyre!("Atomic write task failed: {error}"))?
        .map(|()| AtomicWriteOutcome::Durable)
}

#[cfg(not(windows))]
pub(crate) fn atomic_write_sync(path: &Path, content: &[u8]) -> Result<()> {
    atomic_write_sync_with_parent_sync(
        path,
        content,
        &temp_write_path(path),
        sync_parent_directory_sync,
    )
}

#[cfg(not(windows))]
fn atomic_write_sync_with_parent_sync(
    path: &Path,
    content: &[u8],
    temp_path: &Path,
    sync_parent: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| eyre!("Cannot atomically write path without a parent: {path:?}"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create directory: {parent:?}"))?;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
        .with_context(|| format!("Failed to create temp file: {temp_path:?}"))?;
    let result: Result<()> = (|| {
        file.write_all(content)
            .with_context(|| format!("Failed to write temp file: {temp_path:?}"))?;
        file.flush()
            .with_context(|| format!("Failed to flush temp file: {temp_path:?}"))?;
        file.sync_all()
            .with_context(|| format!("Failed to sync temp file: {temp_path:?}"))?;
        drop(file);
        std::fs::rename(temp_path, path)
            .with_context(|| format!("Failed to rename temp file to: {path:?}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp_path);
    }
    result?;
    sync_parent(parent)
}

#[cfg(windows)]
pub(crate) fn atomic_write_sync(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| eyre!("Cannot atomically write path without a parent: {path:?}"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create directory: {parent:?}"))?;
    let mut file = atomic_write_file::AtomicWriteFile::open(path)
        .with_context(|| format!("Failed to create atomic file for: {path:?}"))?;
    file.write_all(content)
        .with_context(|| format!("Failed to write atomic file for: {path:?}"))?;
    file.commit()
        .with_context(|| format!("Failed to replace file atomically: {path:?}"))?;
    Ok(())
}

#[cfg(unix)]
pub(crate) async fn sync_parent_directory(parent: &Path) -> Result<()> {
    let parent = parent.to_path_buf();
    tokio::task::spawn_blocking(move || sync_parent_directory_sync(&parent))
        .await
        .map_err(|error| eyre!("Parent directory sync task failed: {error}"))??;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) async fn sync_parent_directory(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory_sync(parent: &Path) -> Result<()> {
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("Failed to sync parent directory: {parent:?}"))
}

#[cfg(not(any(unix, windows)))]
fn sync_parent_directory_sync(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(any(not(windows), test))]
pub(crate) fn temp_write_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("state"));
    path.with_file_name(format!(".{file_name}.{}.tmp", Ulid::generate()))
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use color_eyre::eyre::ensure;

    #[test]
    fn sync_failure_does_not_remove_a_reused_temporary_path() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("kraai-atomic-sync-owner-{}", Ulid::generate()));
        let path = directory.join("state.json");
        let temp_path = directory.join("state.tmp");
        let result = atomic_write_sync_with_parent_sync(&path, b"replacement", &temp_path, |_| {
            std::fs::write(&temp_path, b"another writer")?;
            Err(eyre!("injected parent sync failure"))
        });
        let replacement = std::fs::read(&path)?;
        let temporary = std::fs::read(&temp_path)?;
        std::fs::remove_dir_all(directory)?;
        let error = result
            .err()
            .ok_or_else(|| eyre!("expected parent sync failure"))?;
        ensure!(error.to_string() == "injected parent sync failure");
        ensure!(replacement == b"replacement");
        ensure!(temporary == b"another writer");
        Ok(())
    }

    #[tokio::test]
    async fn directory_sync_failure_retains_the_replacement_and_its_outcome() -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("kraai-atomic-outcome-{}", Ulid::generate()));
        let path = directory.join("state.json");
        atomic_write(&path, b"original").await?;

        let outcome = atomic_write_with_parent_sync(&path, b"replacement", |_| async {
            Err(eyre!("injected parent sync failure"))
        })
        .await?;

        ensure!(fs::read(&path).await? == b"replacement");
        let error = outcome
            .into_result()
            .err()
            .ok_or_else(|| eyre!("expected sync failure after replacement"))?;
        ensure!(error.to_string() == "injected parent sync failure");
        let mut entries = fs::read_dir(&directory).await?;
        ensure!(entries.next_entry().await?.is_some());
        ensure!(entries.next_entry().await?.is_none());
        fs::remove_dir_all(directory).await?;
        Ok(())
    }
}
