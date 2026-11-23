mod atomic_write;
mod edits;
mod error;

pub use edits::{ExactTextEdit, apply_exact_edits};
pub use error::WorkspaceFsError;

use kraai_io::fs::read_regular_text_file;

#[cfg(windows)]
mod windows;

use std::path::{Component, Path, PathBuf};

use atomic_write::{WriteMode, atomic_write};

pub fn resolve_path(cwd: &Path, requested: &Path) -> PathBuf {
    if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        cwd.join(requested)
    }
}

pub fn normalize_allow_missing(cwd: &Path, requested: &Path) -> PathBuf {
    let absolute = resolve_path(cwd, requested);
    if let Ok(canonical) = canonicalize(cwd, &absolute) {
        return canonical;
    }

    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn canonicalize(cwd: &Path, path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    return path.canonicalize().or_else(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            windows::canonicalize(cwd, path)
        } else {
            Err(error)
        }
    });
    #[cfg(not(windows))]
    {
        let _ = cwd;
        path.canonicalize()
    }
}

pub fn validate_text_file(cwd: &Path, requested: &Path) -> Result<PathBuf, WorkspaceFsError> {
    read_validated_text_file(cwd, requested).map(|(path, _)| path)
}

fn read_validated_text_file(
    cwd: &Path,
    requested: &Path,
) -> Result<(PathBuf, String), WorkspaceFsError> {
    let path = resolve_path(cwd, requested);
    let canonical = canonicalize(cwd, &path).map_err(|source| WorkspaceFsError::Canonicalize {
        path: path.clone(),
        source,
    })?;
    let metadata = canonical
        .metadata()
        .map_err(|source| WorkspaceFsError::Metadata {
            path: canonical.clone(),
            source,
        })?;
    if !metadata.is_file() {
        return Err(WorkspaceFsError::NotFile(canonical));
    }
    let contents =
        read_regular_text_file(&canonical).map_err(|source| WorkspaceFsError::ReadText {
            path: canonical.clone(),
            source,
        })?;
    Ok((canonical, contents))
}

#[derive(Debug)]
pub struct Written {
    pub path: PathBuf,
    pub unsynced: Option<std::io::Error>,
}

pub fn create_text_file(
    cwd: &Path,
    requested: &Path,
    contents: &str,
) -> Result<Written, WorkspaceFsError> {
    let path = file_destination(cwd, requested)?;
    let unsynced = atomic_write(&path, contents.as_bytes(), WriteMode::Create)?;
    Ok(Written { path, unsynced })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overwrite {
    Created,
    Replaced,
}

/// If another writer creates the file between the check and the write, its file is replaced.
pub fn overwrite_text_file(
    cwd: &Path,
    requested: &Path,
    contents: &str,
) -> Result<(Written, Overwrite), WorkspaceFsError> {
    let destination = file_destination(cwd, requested)?;
    let canonical = |destination: &Path| {
        canonicalize(cwd, destination).map_err(|source| WorkspaceFsError::Canonicalize {
            path: destination.to_path_buf(),
            source,
        })
    };
    match canonicalize(cwd, &destination) {
        Ok(existing) => replace_whole_file(existing, contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match atomic_write(&destination, contents.as_bytes(), WriteMode::Create) {
                Ok(unsynced) => {
                    let written = Written {
                        path: destination,
                        unsynced,
                    };
                    Ok((written, Overwrite::Created))
                }
                Err(WorkspaceFsError::Write { source, .. })
                    if source.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    replace_whole_file(canonical(&destination)?, contents)
                }
                Err(error) => Err(error),
            }
        }
        Err(source) => Err(WorkspaceFsError::Canonicalize {
            path: destination,
            source,
        }),
    }
}

fn replace_whole_file(
    existing: PathBuf,
    contents: &str,
) -> Result<(Written, Overwrite), WorkspaceFsError> {
    let metadata = existing
        .metadata()
        .map_err(|source| WorkspaceFsError::Metadata {
            path: existing.clone(),
            source,
        })?;
    if !metadata.is_file() {
        return Err(WorkspaceFsError::NotFile(existing));
    }
    let unsynced = atomic_write(
        &existing,
        contents.as_bytes(),
        WriteMode::Replace {
            permissions: metadata.permissions(),
        },
    )?;
    let written = Written {
        path: existing,
        unsynced,
    };
    Ok((written, Overwrite::Replaced))
}

fn file_destination(cwd: &Path, requested: &Path) -> Result<PathBuf, WorkspaceFsError> {
    let requested = resolve_path(cwd, requested);
    let name = requested
        .file_name()
        .ok_or_else(|| WorkspaceFsError::MissingFileName(requested.clone()))?;
    let parent = requested
        .parent()
        .ok_or_else(|| WorkspaceFsError::MissingParent(requested.clone()))?;
    let parent = canonicalize(cwd, parent).map_err(|source| WorkspaceFsError::Canonicalize {
        path: parent.to_path_buf(),
        source,
    })?;
    if !parent.is_dir() {
        return Err(WorkspaceFsError::NotDirectory(parent));
    }
    Ok(parent.join(name))
}

pub fn edit_text_file(
    cwd: &Path,
    requested: &Path,
    edits: &[ExactTextEdit],
) -> Result<Written, WorkspaceFsError> {
    if edits.is_empty() {
        return Err(WorkspaceFsError::NoEdits);
    }
    let (path, original) = read_validated_text_file(cwd, requested)?;
    let updated = apply_exact_edits(&path, &original, edits)?;
    let permissions = path
        .metadata()
        .map_err(|source| WorkspaceFsError::Metadata {
            path: path.clone(),
            source,
        })?
        .permissions();
    let unsynced = atomic_write(
        &path,
        updated.as_bytes(),
        WriteMode::Replace { permissions },
    )?;
    Ok(Written { path, unsynced })
}

#[cfg(test)]
mod tests;
