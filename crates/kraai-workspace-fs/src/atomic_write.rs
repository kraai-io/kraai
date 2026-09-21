#[cfg(not(windows))]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use ulid::Ulid;

use crate::WorkspaceFsError;
#[cfg(windows)]
use crate::windows;

pub(super) enum WriteMode {
    Create,
    Replace { permissions: fs::Permissions },
}

pub(super) fn atomic_write(
    path: &Path,
    contents: &[u8],
    mode: WriteMode,
) -> Result<(), WorkspaceFsError> {
    let parent = path
        .parent()
        .ok_or_else(|| WorkspaceFsError::MissingParent(path.to_path_buf()))?;
    path.file_name()
        .ok_or_else(|| WorkspaceFsError::MissingFileName(path.to_path_buf()))?;
    let temp_path = parent.join(format!(".kraai-{}.tmp", Ulid::generate()));
    write_atomic_file(path, parent, &temp_path, contents, mode, sync_directory)
}

fn write_atomic_file(
    path: &Path,
    parent: &Path,
    temp_path: &Path,
    contents: &[u8],
    mode: WriteMode,
    sync_parent: impl FnOnce(&Path) -> Result<(), WorkspaceFsError>,
) -> Result<(), WorkspaceFsError> {
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
        .map_err(|source| WorkspaceFsError::Write {
            path: temp_path.to_path_buf(),
            source,
        })?;
    let result: Result<(), WorkspaceFsError> = (|| {
        if let WriteMode::Replace { permissions } = &mode {
            temp.set_permissions(permissions.clone())
                .map_err(|source| WorkspaceFsError::Write {
                    path: temp_path.to_path_buf(),
                    source,
                })?;
        }
        temp.write_all(contents)
            .and_then(|()| temp.flush())
            .and_then(|()| temp.sync_all())
            .map_err(|source| WorkspaceFsError::Write {
                path: temp_path.to_path_buf(),
                source,
            })?;
        drop(temp);

        match mode {
            WriteMode::Create => rename_without_replacement(temp_path, path)?,
            WriteMode::Replace { .. } => {
                replace_file(temp_path, path).map_err(|source| WorkspaceFsError::Write {
                    path: path.to_path_buf(),
                    source,
                })?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp_path);
    }
    result?;
    sync_parent(parent)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_without_replacement(source: &Path, destination: &Path) -> Result<(), WorkspaceFsError> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|source| WorkspaceFsError::Write {
        path: destination.to_path_buf(),
        source: std::io::Error::from_raw_os_error(source.raw_os_error()),
    })
}

#[cfg(windows)]
fn rename_without_replacement(source: &Path, destination: &Path) -> Result<(), WorkspaceFsError> {
    windows::rename(source, destination, false).map_err(|source| WorkspaceFsError::Write {
        path: destination.to_path_buf(),
        source,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rename_without_replacement(source: &Path, destination: &Path) -> Result<(), WorkspaceFsError> {
    if destination.exists() {
        return Err(WorkspaceFsError::AlreadyExists(destination.to_path_buf()));
    }
    fs::rename(source, destination).map_err(|source| WorkspaceFsError::Write {
        path: destination.to_path_buf(),
        source,
    })
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    windows::rename(source, destination, true)
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(not(windows))]
fn sync_directory(path: &Path) -> Result<(), WorkspaceFsError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| WorkspaceFsError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> Result<(), WorkspaceFsError> {
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "filesystem unit tests use direct fixture and output assertions"
)]
mod tests {
    use super::*;
    use crate::tests::temp_dir;

    #[test]
    fn atomic_write_cleans_up_only_temporary_files_it_created() {
        let root = temp_dir("temp-file-ownership");
        let destination = root.join("file.txt");
        let temp_path = root.join("existing.tmp");
        fs::write(&destination, "original").unwrap();
        fs::write(&temp_path, "owned by another writer").unwrap();

        let error = write_atomic_file(
            &destination,
            &root,
            &temp_path,
            b"replacement",
            WriteMode::Replace {
                permissions: destination.metadata().unwrap().permissions(),
            },
            sync_directory,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            WorkspaceFsError::Write { path, source }
                if path == temp_path && source.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(fs::read_to_string(&destination).unwrap(), "original");
        assert_eq!(
            fs::read_to_string(&temp_path).unwrap(),
            "owned by another writer"
        );

        fs::remove_file(&temp_path).unwrap();
        assert!(
            write_atomic_file(
                &destination,
                &root,
                &temp_path,
                b"replacement",
                WriteMode::Create,
                sync_directory
            )
            .is_err()
        );
        assert!(!temp_path.exists());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "original");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sync_failure_does_not_remove_a_reused_temporary_path() {
        let root = temp_dir("sync-temp-ownership");
        let destination = root.join("file.txt");
        let temp_path = root.join("file.tmp");
        let error = write_atomic_file(
            &destination,
            &root,
            &temp_path,
            b"replacement",
            WriteMode::Create,
            |_| {
                fs::write(&temp_path, "owned by another writer").map_err(|source| {
                    WorkspaceFsError::Write {
                        path: temp_path.clone(),
                        source,
                    }
                })?;
                Err(WorkspaceFsError::Write {
                    path: root.clone(),
                    source: std::io::Error::other("injected parent sync failure"),
                })
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            WorkspaceFsError::Write { path, source }
                if path == root && source.to_string() == "injected parent sync failure"
        ));
        assert_eq!(fs::read_to_string(&destination).unwrap(), "replacement");
        assert_eq!(
            fs::read_to_string(&temp_path).unwrap(),
            "owned by another writer"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_create_preserves_a_destination_created_after_validation() {
        let root = temp_dir("atomic-create");
        let path = root.join("file.txt");
        atomic_write(&path, b"original", WriteMode::Create).unwrap();
        assert!(atomic_write(&path, b"replacement", WriteMode::Create).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        let _ = fs::remove_dir_all(root);
    }
}
