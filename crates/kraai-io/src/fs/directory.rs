use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

mod bootstrap;

pub use bootstrap::DirectoryBootstrap;

pub fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, open};
        let directory = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        fs::File::from(directory).sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Accepts the nearest existing ancestor as durable, so later writes should use
/// `create_dir_all_in`.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    let path = nonempty_path(path);
    let mut anchor = path;
    loop {
        match fs::metadata(anchor) {
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => return Err(io::ErrorKind::NotADirectory.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                anchor = parent_directory(anchor)?;
            }
            Err(error) => return Err(error),
        }
    }
    if anchor == path {
        return Ok(());
    }
    create_directories_with_sync(anchor, path, false, sync_directory)
}

/// The existing anchor and its ancestors must already be durable.
pub fn create_dir_all_in(anchor: &Path, path: &Path) -> io::Result<()> {
    create_directories_with_sync(anchor, path, false, sync_directory)
}

pub fn create_private_dir_all_in(anchor: &Path, path: &Path) -> io::Result<()> {
    create_directories_with_sync(anchor, path, true, sync_directory)
}

fn create_directories_with_sync(
    anchor: &Path,
    path: &Path,
    private: bool,
    mut sync: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let directories = directory_chain(anchor, path)?;
    if !fs::metadata(nonempty_path(anchor))?.is_dir() {
        return Err(io::ErrorKind::NotADirectory.into());
    }
    #[cfg(windows)]
    if private {
        for directory in directories.iter().skip(1) {
            match super::owner_only::create_private_dir(directory) {
                Err(error)
                    if error.kind() == io::ErrorKind::AlreadyExists
                        && fs::metadata(directory)?.is_dir() => {}
                result => result?,
            }
        }
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = private;
    builder.create(nonempty_path(path))?;
    for directory in directories.iter().rev() {
        sync(directory)?;
    }
    Ok(())
}

fn directory_chain(anchor: &Path, path: &Path) -> io::Result<Vec<PathBuf>> {
    let anchor = nonempty_path(anchor);
    let path = nonempty_path(path);
    let relative = if anchor == Path::new(".") && !path.is_absolute() {
        path.trim_prefix(".")
    } else {
        path.strip_prefix(anchor).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("directory is outside its anchor: {error}"),
            )
        })?
    };
    let mut current = anchor.to_path_buf();
    let mut directories = vec![current.clone()];
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path traverses outside its anchor",
            ));
        };
        current.push(name);
        directories.push(current.clone());
    }
    Ok(directories)
}

fn nonempty_path(path: &Path) -> &Path {
    if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    }
}

pub(super) fn parent_directory(path: &Path) -> io::Result<&Path> {
    path.parent()
        .map(nonempty_path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory"))
}

pub fn remove_file_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn remove_file_durable(path: &Path) -> io::Result<bool> {
    let removed = remove_file_if_exists(path)?;
    match sync_directory(parent_directory(path)?) {
        Ok(()) => Ok(removed),
        Err(error) if !removed && error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn remove_dir_all_durable(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => sync_directory(parent_directory(path)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match sync_directory(parent_directory(path)?) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(feature = "async")]
pub async fn sync_directory_async(path: &Path) -> io::Result<()> {
    run_path_operation(path, sync_directory).await
}

#[cfg(feature = "async")]
pub async fn create_dir_all_async(path: &Path) -> io::Result<()> {
    run_path_operation(path, create_dir_all).await
}

#[cfg(feature = "async")]
pub async fn create_dir_all_in_async(anchor: &Path, path: &Path) -> io::Result<()> {
    run_anchored_operation(anchor, path, create_dir_all_in).await
}

#[cfg(feature = "async")]
pub async fn create_private_dir_all_in_async(anchor: &Path, path: &Path) -> io::Result<()> {
    run_anchored_operation(anchor, path, create_private_dir_all_in).await
}

#[cfg(feature = "async")]
pub async fn remove_file_if_exists_async(path: &Path) -> io::Result<bool> {
    run_path_operation(path, remove_file_if_exists).await
}

#[cfg(feature = "async")]
pub async fn remove_file_durable_async(path: &Path) -> io::Result<bool> {
    run_path_operation(path, remove_file_durable).await
}

#[cfg(feature = "async")]
pub async fn remove_dir_all_durable_async(path: &Path) -> io::Result<()> {
    run_path_operation(path, remove_dir_all_durable).await
}

#[cfg(feature = "async")]
async fn run_path_operation<T: Send + 'static>(
    path: &Path,
    operation: fn(&Path) -> io::Result<T>,
) -> io::Result<T> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || operation(&path))
        .await
        .map_err(io::Error::other)?
}

#[cfg(feature = "async")]
async fn run_anchored_operation(
    anchor: &Path,
    path: &Path,
    operation: fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let anchor = anchor.to_path_buf();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || operation(&anchor, &path))
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests;
