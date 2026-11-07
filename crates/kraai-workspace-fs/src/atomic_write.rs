use std::fs;
use std::path::Path;

use kraai_io::fs::{AtomicWriteOutcome, atomic_create, atomic_replace_preserving};

use crate::WorkspaceFsError;

pub(super) enum WriteMode {
    Create,
    Replace { permissions: fs::Permissions },
}

/// Publishes `contents` at `path`. `Ok(Some(error))` means the file was published but its parent
/// directory could not be synced.
pub(super) fn atomic_write(
    path: &Path,
    contents: &[u8],
    mode: WriteMode,
) -> Result<Option<std::io::Error>, WorkspaceFsError> {
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
    match result {
        Ok(AtomicWriteOutcome::Durable) => Ok(None),
        Ok(AtomicWriteOutcome::ReplacedButNotSynced(error)) => Ok(Some(error)),
        Err(source) => Err(WorkspaceFsError::Write {
            path: path.to_path_buf(),
            source,
        }),
    }
}
