#![deny(unsafe_code)]

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
#[expect(
    clippy::unwrap_used,
    reason = "filesystem unit tests use direct fixture and output assertions"
)]
mod tests {
    use std::fs;

    use ulid::Ulid;

    use super::*;

    pub(super) fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("kraai-workspace-fs-{name}-{}", Ulid::generate()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn edit_preserves_trailing_newline_and_permissions() {
        let directory = temp_dir("edit");
        let path = directory.join("file.txt");
        fs::write(&path, "alpha\nbeta\n").unwrap();
        let permissions = path.metadata().unwrap().permissions();
        edit_text_file(
            &directory,
            Path::new("file.txt"),
            &[ExactTextEdit {
                start_line: 2,
                end_line: 2,
                old_text: String::from("beta\n"),
                new_text: String::from("gamma\n"),
            }],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\ngamma\n");
        assert_eq!(path.metadata().unwrap().permissions(), permissions);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn create_never_replaces_an_existing_file() {
        let directory = temp_dir("create");
        let path = directory.join("file.txt");
        fs::write(&path, "original").unwrap();
        let error = create_text_file(&directory, Path::new("file.txt"), "replacement").unwrap_err();
        assert!(matches!(
            error,
            WorkspaceFsError::Write { ref source, .. }
                if source.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn overwrite_creates_or_replaces_and_keeps_permissions() {
        let directory = temp_dir("overwrite");
        let path = directory.join("file.txt");
        let (created, outcome) =
            overwrite_text_file(&directory, Path::new("file.txt"), "first\n").unwrap();
        assert_eq!(outcome, Overwrite::Created);
        assert_eq!(
            created.path,
            directory.canonicalize().unwrap().join("file.txt")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let permissions = path.metadata().unwrap().permissions();

        let (_, outcome) =
            overwrite_text_file(&directory, Path::new("file.txt"), "second\n").unwrap();
        assert_eq!(outcome, Overwrite::Replaced);
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        assert_eq!(path.metadata().unwrap().permissions(), permissions);

        fs::create_dir(directory.join("folder")).unwrap();
        let error = overwrite_text_file(&directory, Path::new("folder"), "x").unwrap_err();
        assert!(matches!(error, WorkspaceFsError::NotFile(_)));
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn create_and_edit_support_a_maximum_length_filename() {
        let directory = temp_dir("long-filename");
        let path = directory.join("x".repeat(255));
        fs::write(&path, "valid filename").unwrap();
        fs::remove_file(&path).unwrap();

        create_text_file(&directory, &path, "original\n").unwrap();
        edit_text_file(
            &directory,
            &path,
            &[ExactTextEdit {
                start_line: 1,
                end_line: 1,
                old_text: String::from("original\n"),
                new_text: String::from("replacement\n"),
            }],
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement\n");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn regular_reads_preserve_text_and_io_errors() {
        let root = temp_dir("regular-read");
        let path = root.join("file.txt");
        for contents in ["", "alpha\nβeta\n", "last line"] {
            fs::write(&path, contents).unwrap();
            assert_eq!(read_regular_text_file(&path).unwrap(), contents);
            assert_eq!(
                read_validated_text_file(&root, Path::new("file.txt"))
                    .unwrap()
                    .1,
                contents
            );
        }
        fs::write(&path, [0xff, 0xfe]).unwrap();
        let actual = read_regular_text_file(&path).unwrap_err();
        let expected = fs::read_to_string(&path).unwrap_err();
        assert_eq!(actual.kind(), expected.kind());
        assert_eq!(actual.to_string(), expected.to_string());
        assert!(matches!(
            validate_text_file(&root, Path::new("file.txt")),
            Err(WorkspaceFsError::ReadText { .. })
        ));
        fs::remove_file(&path).unwrap();
        let actual = read_regular_text_file(&path).unwrap_err();
        let expected = fs::read_to_string(&path).unwrap_err();
        assert_eq!(actual.kind(), expected.kind());
        assert_eq!(actual.to_string(), expected.to_string());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn resolves_opened_paths_without_volume_manager_access() {
        let root = temp_dir("windows-final-path").canonicalize().unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        let file = root.join("résolved.txt");
        fs::write(&file, "contents").unwrap();
        let resolved = windows::canonicalize(&root, &root.join("nested/../résolved.txt")).unwrap();
        assert_eq!(resolved, file.canonicalize().unwrap());
        assert_eq!(
            windows::canonicalize(&root.join("nested"), &file).unwrap(),
            resolved
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalizes_missing_paths_without_requiring_the_file_to_exist() {
        assert_eq!(
            normalize_allow_missing(Path::new("/workspace/src"), Path::new("../missing.rs")),
            Path::new("/workspace/missing.rs")
        );
    }
}
