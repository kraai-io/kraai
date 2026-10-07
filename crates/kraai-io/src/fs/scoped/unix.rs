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
    open_file_with_openat2(root, relative, path, policy, |resolve| {
        rustix::fs::openat2(
            root,
            relative,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
            resolve,
        )
    })
}

#[cfg(target_os = "linux")]
fn open_file_with_openat2(
    root: &File,
    relative: &Path,
    path: &Path,
    policy: SymlinkPolicy,
    try_open: impl FnOnce(rustix::fs::ResolveFlags) -> rustix::io::Result<rustix::fd::OwnedFd>,
) -> Result<File, ScopedReadError> {
    use std::os::unix::ffi::OsStrExt;

    use rustix::fs::ResolveFlags;
    use rustix::io::Errno;

    let mut resolve = ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS;
    if policy == SymlinkPolicy::Reject {
        validate_components(relative, path)?;
        resolve |= ResolveFlags::NO_SYMLINKS;
    }
    if relative.as_os_str().as_bytes().contains(&0) {
        return Err(open_error(Errno::INVAL, path));
    }
    match try_open(resolve) {
        Ok(descriptor) => validate_file(File::from(descriptor), path),
        Err(Errno::NOSYS | Errno::PERM | Errno::INVAL) if policy == SymlinkPolicy::Reject => {
            open_file_walk(root, relative, path)
        }
        Err(error) => Err(open_error(error, path)),
    }
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
mod tests;
