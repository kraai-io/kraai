use std::collections::BTreeSet;
#[cfg(unix)]
use std::ffi::OsStr;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use crate::SandboxError;

use super::PROTECTED_METADATA_NAMES;

pub(super) fn protected_paths(workspace: &Path) -> Result<BTreeSet<PathBuf>, SandboxError> {
    let workspace = resolve(workspace)?;
    let mut paths = BTreeSet::new();
    for name in PROTECTED_METADATA_NAMES {
        let path = workspace.join(name);
        paths.insert(path.clone());
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(invalid(&path, error)),
        }
        let resolved = resolve_metadata(&path, &workspace, Some(&path), &mut 0)?;
        paths.insert(resolved.clone());
        if *name == ".git" {
            protect_git_paths(&path, resolved, &workspace, &mut paths).map_err(|_error| {
                invalid(
                    &path,
                    "linked Git metadata is unavailable or uses an unsupported symlink",
                )
            })?;
        }
    }
    Ok(paths)
}

fn protect_git_paths(
    path: &Path,
    resolved: PathBuf,
    workspace: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), SandboxError> {
    let git_dir = if resolved.is_dir() {
        resolved
    } else if let Some(target) = read_pointer(path, b"gitdir: ")? {
        let target = resolve_metadata(&target, workspace, None, &mut 0)?;
        if !target.is_dir() {
            return Err(invalid(&target, "Git metadata target must be a directory"));
        }
        paths.insert(target.clone());
        target
    } else {
        return Ok(());
    };
    let common_pointer = git_dir.join("commondir");
    if let Some(common_dir) = read_pointer(&common_pointer, b"")? {
        paths.insert(resolve_metadata(
            &common_pointer,
            workspace,
            Some(&common_pointer),
            &mut 0,
        )?);
        let common_dir = resolve_metadata(&common_dir, workspace, None, &mut 0)?;
        if !common_dir.is_dir() {
            return Err(invalid(
                &common_dir,
                "Git common directory must be a directory",
            ));
        }
        paths.insert(common_dir);
    }
    Ok(())
}

fn read_pointer(path: &Path, prefix: &[u8]) -> Result<Option<PathBuf>, SandboxError> {
    const MAX_POINTER_BYTES: u64 = 64 * 1024;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(path)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                return Ok(None);
            }
            return Err(invalid(path, error));
        }
        Err(error) => return Err(invalid(path, error)),
    };
    if !file
        .metadata()
        .map_err(|error| invalid(path, error))?
        .is_file()
    {
        return Err(invalid(path, "Git metadata pointer must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_POINTER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| invalid(path, error))?;
    if bytes.len() as u64 > MAX_POINTER_BYTES {
        return Err(invalid(path, "Git metadata pointer is too large"));
    }
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bytes.pop();
    }
    let Some(value) = bytes.strip_prefix(prefix) else {
        return Ok(None);
    };
    if value.is_empty() || value.contains(&0) {
        return Err(invalid(path, "Git metadata pointer has an invalid path"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid(path, "metadata pointer has no parent"))?;
    #[cfg(unix)]
    let target = OsStr::from_bytes(value);
    #[cfg(not(unix))]
    let target = std::str::from_utf8(value)
        .map_err(|_| invalid(path, "Git metadata pointer must contain a UTF-8 path"))?;
    Ok(Some(parent.join(target)))
}

fn resolve(path: &Path) -> Result<PathBuf, SandboxError> {
    path.canonicalize().map_err(|error| invalid(path, error))
}

fn resolve_metadata(
    path: &Path,
    workspace: &Path,
    permitted_alias: Option<&Path>,
    followed_links: &mut usize,
) -> Result<PathBuf, SandboxError> {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => resolved.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                if !std::fs::symlink_metadata(&resolved)
                    .map_err(|error| invalid(&resolved, error))?
                    .file_type()
                    .is_symlink()
                {
                    continue;
                }
                *followed_links += 1;
                if *followed_links > 40 {
                    return Err(invalid(path, "too many metadata symlinks"));
                }
                let parent = resolved
                    .parent()
                    .ok_or_else(|| invalid(path, "metadata symlink has no parent"))?;
                resolved = resolve(parent)?.join(name);
                if resolved.starts_with(workspace) && permitted_alias != Some(resolved.as_path()) {
                    return Err(invalid(
                        &resolved,
                        "indirect metadata symlinks inside the workspace require metadata-write",
                    ));
                }
                let target =
                    std::fs::read_link(&resolved).map_err(|error| invalid(&resolved, error))?;
                resolved.pop();
                resolved = resolve_metadata(
                    &resolved.join(target),
                    workspace,
                    permitted_alias,
                    followed_links,
                )?;
            }
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
        }
    }
    resolve(&resolved)
}

fn invalid(path: &Path, reason: impl std::fmt::Display) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "unable to protect metadata '{}': {reason}",
        path.display()
    ))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "metadata fixtures require filesystem setup"
)]
mod tests;
