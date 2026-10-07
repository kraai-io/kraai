use std::fs::File;
use std::path::{Component, Path};

use rustix::fs::{Mode, OFlags, open};

use super::{ScopedReadError, SymlinkPolicy, validate_file};

pub(super) fn open_root(root: &Path) -> Result<File, ScopedReadError> {
    let flags =
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NONBLOCK | OFlags::NOFOLLOW;
    let descriptor =
        open(root, flags, Mode::empty()).map_err(|error| ScopedReadError::OpenRoot {
            path: root.to_path_buf(),
            source: error.into(),
        })?;
    Ok(File::from(descriptor))
}

#[cfg(target_os = "linux")]
pub(super) fn open_file(
    root: &File,
    relative: &Path,
    path: &Path,
    policy: SymlinkPolicy,
) -> Result<File, ScopedReadError> {
    use rustix::fs::{ResolveFlags, openat2};

    let mut resolve = ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS;
    if policy == SymlinkPolicy::Reject {
        validate_components(relative, path)?;
        resolve |= ResolveFlags::NO_SYMLINKS;
    }
    let descriptor = openat2(
        root,
        relative,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
        resolve,
    )
    .map_err(|error| open_error(error, path))?;
    validate_file(File::from(descriptor), path)
}

#[cfg(target_os = "macos")]
pub(super) fn open_file(
    root: &File,
    relative: &Path,
    path: &Path,
    _policy: SymlinkPolicy,
) -> Result<File, ScopedReadError> {
    open_file_walk(root, relative, path)
}

fn validate_components(relative: &Path, path: &Path) -> Result<(), ScopedReadError> {
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ScopedReadError::OutsideRoot(path.to_path_buf()));
    }
    Ok(())
}

fn open_error(error: rustix::io::Errno, path: &Path) -> ScopedReadError {
    match error {
        rustix::io::Errno::NOENT => ScopedReadError::NotFound(path.to_path_buf()),
        rustix::io::Errno::XDEV | rustix::io::Errno::LOOP => {
            ScopedReadError::OutsideRoot(path.to_path_buf())
        }
        rustix::io::Errno::NOTDIR if cfg!(target_os = "macos") => {
            ScopedReadError::NotFile(path.to_path_buf())
        }
        _ => ScopedReadError::Open {
            path: path.to_path_buf(),
            source: error.into(),
        },
    }
}

#[cfg(any(target_os = "macos", test))]
fn open_file_walk(root: &File, relative: &Path, path: &Path) -> Result<File, ScopedReadError> {
    use rustix::fs::openat;

    validate_components(relative, path)?;
    let mut components = relative.components().peekable();
    let mut directory = None;
    while let Some(component) = components.next() {
        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW;
        if components.peek().is_some() {
            flags |= OFlags::DIRECTORY;
        }
        let descriptor = openat(
            directory.as_ref().unwrap_or(root),
            component.as_os_str(),
            flags,
            Mode::empty(),
        )
        .map_err(|error| open_error(error, path))?;
        directory = Some(File::from(descriptor));
    }
    validate_file(
        directory.ok_or_else(|| ScopedReadError::NotFile(path.to_path_buf()))?,
        path,
    )
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "scoped walk tests assert fixture operations"
)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn fallback_walk_rejects_symlinks_and_reads_nested_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = open_root(directory.path()).unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested/file"), b"inside").unwrap();
        assert!(open_file_walk(&root, Path::new("nested/file"), directory.path()).is_ok());
        assert!(matches!(
            open_file_walk(&root, Path::new("../outside"), directory.path()),
            Err(ScopedReadError::OutsideRoot(_))
        ));
        symlink("nested", directory.path().join("alias")).unwrap();
        assert!(open_file_walk(&root, Path::new("alias/file"), directory.path()).is_err());
        symlink("file", directory.path().join("nested/link")).unwrap();
        assert!(open_file_walk(&root, Path::new("nested/link"), directory.path()).is_err());
        nix::unistd::mkfifo(
            directory.path().join("fifo").as_path(),
            nix::sys::stat::Mode::S_IRWXU,
        )
        .unwrap();
        assert!(matches!(
            open_file_walk(&root, Path::new("fifo"), directory.path()),
            Err(ScopedReadError::NotFile(_))
        ));
    }
}
