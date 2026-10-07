use std::fs;
use std::path::Path;

use kraai_io::fs::{atomic_create, atomic_replace_preserving};

use crate::WorkspaceFsError;

pub(super) enum WriteMode {
    Create,
    Replace { permissions: fs::Permissions },
}

pub(super) fn atomic_write(
    path: &Path,
    contents: &[u8],
    mode: WriteMode,
) -> Result<(), WorkspaceFsError> {
    path.parent()
        .ok_or_else(|| WorkspaceFsError::MissingParent(path.to_path_buf()))?;
    path.file_name()
        .ok_or_else(|| WorkspaceFsError::MissingFileName(path.to_path_buf()))?;
    let result = match mode {
        WriteMode::Create => atomic_create(path, contents),
        WriteMode::Replace { permissions } => {
            atomic_replace_preserving(path, contents, permissions)
        }
    };
    result
        .and_then(|outcome| outcome.into_result())
        .map_err(|source| WorkspaceFsError::Write {
            path: path.to_path_buf(),
            source,
        })
}
