use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::directory::{
    create_dir_all_in, create_private_dir_all_in, parent_directory, sync_directory,
};

#[cfg(windows)]
mod windows;

/// Replacement has happened for either outcome. An outer error means publication failed.
/// Directory syncing is supported on Unix; Windows uses write-through replacement.
#[derive(Debug)]
pub enum AtomicWriteOutcome {
    Durable,
    ReplacedButNotSynced(io::Error),
}

impl AtomicWriteOutcome {
    pub fn into_result(self) -> io::Result<()> {
        match self {
            Self::Durable => Ok(()),
            Self::ReplacedButNotSynced(error) => Err(error),
        }
    }
}

enum WriteMode {
    Create,
    Replace,
    Preserve(Permissions),
    Private,
}

/// Replace a file within an existing, durably established parent directory.
pub fn atomic_replace(path: &Path, content: &[u8]) -> io::Result<AtomicWriteOutcome> {
    write_atomic(path, content, WriteMode::Replace, true)
}

pub fn atomic_replace_private(path: &Path, content: &[u8]) -> io::Result<AtomicWriteOutcome> {
    write_atomic(path, content, WriteMode::Private, true)
}

/// Create and sync parent directories beneath an existing durable anchor before replacement.
pub fn atomic_replace_in(
    anchor: &Path,
    path: &Path,
    content: &[u8],
) -> io::Result<AtomicWriteOutcome> {
    create_dir_all_in(anchor, parent_directory(path)?)?;
    atomic_replace(path, content)
}

pub fn atomic_replace_private_in(
    anchor: &Path,
    path: &Path,
    content: &[u8],
) -> io::Result<AtomicWriteOutcome> {
    create_private_dir_all_in(anchor, parent_directory(path)?)?;
    atomic_replace_private(path, content)
}

pub fn atomic_replace_preserving(
    path: &Path,
    content: &[u8],
    permissions: Permissions,
) -> io::Result<AtomicWriteOutcome> {
    write_atomic(path, content, WriteMode::Preserve(permissions), true)
}

pub fn atomic_create(path: &Path, content: &[u8]) -> io::Result<AtomicWriteOutcome> {
    write_atomic(path, content, WriteMode::Create, true)
}

pub fn atomic_replace_unsynced(path: &Path, content: &[u8]) -> io::Result<()> {
    write_atomic(path, content, WriteMode::Replace, false)?.into_result()
}

pub fn atomic_replace_private_unsynced(path: &Path, content: &[u8]) -> io::Result<()> {
    write_atomic(path, content, WriteMode::Private, false)?.into_result()
}

fn write_atomic(
    path: &Path,
    content: &[u8],
    mode: WriteMode,
    durable: bool,
) -> io::Result<AtomicWriteOutcome> {
    write_with_sync(
        path,
        content,
        mode,
        durable,
        &temporary_path(path)?,
        sync_directory,
    )
}

fn temporary_path(path: &Path) -> io::Result<PathBuf> {
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path has no file name",
        ));
    }
    Ok(parent_directory(path)?.join(format!(".kraai-{}.tmp", ulid::Ulid::generate())))
}

fn write_with_sync(
    path: &Path,
    content: &[u8],
    mode: WriteMode,
    durable: bool,
    temporary_path: &Path,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<AtomicWriteOutcome> {
    let parent = parent_directory(path)?;
    let mut temporary = TemporaryFile {
        path: temporary_path.to_path_buf(),
        remove_on_drop: false,
    };
    let mut file = open_temporary(temporary_path, &mode)?;
    temporary.remove_on_drop = true;
    if let WriteMode::Preserve(permissions) = &mode {
        file.set_permissions(permissions.clone())?;
    }
    file.write_all(content)?;
    file.flush()?;
    if durable {
        file.sync_all()?;
    }
    drop(file);
    publish(
        temporary_path,
        path,
        matches!(mode, WriteMode::Create),
        durable,
    )?;
    temporary.remove_on_drop = false;
    if durable && let Err(error) = sync_parent(parent) {
        return Ok(AtomicWriteOutcome::ReplacedButNotSynced(error));
    }
    Ok(AtomicWriteOutcome::Durable)
}

fn open_temporary(path: &Path, mode: &WriteMode) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if matches!(mode, WriteMode::Private) {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path)
}

struct TemporaryFile {
    path: PathBuf,
    remove_on_drop: bool,
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn publish(source: &Path, destination: &Path, create: bool, _durable: bool) -> io::Result<()> {
    if create {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            source,
            rustix::fs::CWD,
            destination,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
        Ok(())
    } else {
        fs::rename(source, destination)
    }
}

#[cfg(windows)]
fn publish(source: &Path, destination: &Path, create: bool, durable: bool) -> io::Result<()> {
    windows::rename(source, destination, !create, durable)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn publish(source: &Path, destination: &Path, create: bool, _durable: bool) -> io::Result<()> {
    if create {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic creation is unsupported on this platform",
        ))
    } else {
        fs::rename(source, destination)
    }
}

#[cfg(feature = "async")]
pub async fn atomic_replace_async(path: &Path, content: &[u8]) -> io::Result<AtomicWriteOutcome> {
    let path = path.to_path_buf();
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || atomic_replace(&path, &content))
        .await
        .map_err(io::Error::other)?
}

#[cfg(feature = "async")]
pub async fn atomic_replace_private_async(
    path: &Path,
    content: &[u8],
) -> io::Result<AtomicWriteOutcome> {
    let path = path.to_path_buf();
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || atomic_replace_private(&path, &content))
        .await
        .map_err(io::Error::other)?
}

#[cfg(feature = "async")]
pub async fn atomic_replace_in_async(
    anchor: &Path,
    path: &Path,
    content: &[u8],
) -> io::Result<AtomicWriteOutcome> {
    run_anchored_write(anchor, path, content, atomic_replace_in).await
}

#[cfg(feature = "async")]
pub async fn atomic_replace_private_in_async(
    anchor: &Path,
    path: &Path,
    content: &[u8],
) -> io::Result<AtomicWriteOutcome> {
    run_anchored_write(anchor, path, content, atomic_replace_private_in).await
}

#[cfg(feature = "async")]
async fn run_anchored_write(
    anchor: &Path,
    path: &Path,
    content: &[u8],
    operation: fn(&Path, &Path, &[u8]) -> io::Result<AtomicWriteOutcome>,
) -> io::Result<AtomicWriteOutcome> {
    let anchor = anchor.to_path_buf();
    let path = path.to_path_buf();
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || operation(&anchor, &path, &content))
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "atomic write tests assert publication and durability outcomes"
)]
mod tests;
