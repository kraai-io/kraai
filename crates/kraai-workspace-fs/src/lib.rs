#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use ulid::Ulid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactTextEdit {
    pub start_line: u32,
    pub end_line: u32,
    pub old_text: String,
    pub new_text: String,
}

pub fn resolve_path(cwd: &Path, requested: &Path) -> PathBuf {
    if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        cwd.join(requested)
    }
}

pub fn normalize_allow_missing(cwd: &Path, requested: &Path) -> PathBuf {
    let absolute = resolve_path(cwd, requested);
    if let Ok(canonical) = absolute.canonicalize() {
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

pub fn validate_text_file(cwd: &Path, requested: &Path) -> Result<PathBuf, WorkspaceFsError> {
    let path = resolve_path(cwd, requested);
    let canonical = path
        .canonicalize()
        .map_err(|source| WorkspaceFsError::Canonicalize {
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
    fs::read_to_string(&canonical).map_err(|source| WorkspaceFsError::ReadText {
        path: canonical.clone(),
        source,
    })?;
    Ok(canonical)
}

pub fn create_text_file(
    cwd: &Path,
    requested: &Path,
    contents: &str,
) -> Result<PathBuf, WorkspaceFsError> {
    let requested = resolve_path(cwd, requested);
    let name = requested
        .file_name()
        .ok_or_else(|| WorkspaceFsError::MissingFileName(requested.clone()))?;
    let parent = requested
        .parent()
        .ok_or_else(|| WorkspaceFsError::MissingParent(requested.clone()))?;
    let parent = parent
        .canonicalize()
        .map_err(|source| WorkspaceFsError::Canonicalize {
            path: parent.to_path_buf(),
            source,
        })?;
    if !parent.is_dir() {
        return Err(WorkspaceFsError::NotDirectory(parent));
    }
    let destination = parent.join(name);
    atomic_write(&destination, contents.as_bytes(), WriteMode::Create)?;
    Ok(destination)
}

pub fn edit_text_file(
    cwd: &Path,
    requested: &Path,
    edits: &[ExactTextEdit],
) -> Result<PathBuf, WorkspaceFsError> {
    if edits.is_empty() {
        return Err(WorkspaceFsError::NoEdits);
    }
    let path = validate_text_file(cwd, requested)?;
    let original = fs::read_to_string(&path).map_err(|source| WorkspaceFsError::ReadText {
        path: path.clone(),
        source,
    })?;
    let updated = apply_exact_edits(&path, &original, edits)?;
    let permissions = path
        .metadata()
        .map_err(|source| WorkspaceFsError::Metadata {
            path: path.clone(),
            source,
        })?
        .permissions();
    atomic_write(
        &path,
        updated.as_bytes(),
        WriteMode::Replace { permissions },
    )?;
    Ok(path)
}

pub fn apply_exact_edits(
    path: &Path,
    contents: &str,
    edits: &[ExactTextEdit],
) -> Result<String, WorkspaceFsError> {
    let lines = index_lines(contents);
    let mut pending = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        pending.push(validate_edit(path, contents, &lines, index, edit)?);
    }

    pending.sort_by_key(|edit| (edit.start_line, edit.end_line));
    for window in pending.windows(2) {
        let [previous, current] = window else {
            continue;
        };
        if current.start_line <= previous.end_line {
            return Err(WorkspaceFsError::OverlappingEdits {
                path: path.to_path_buf(),
                first_start: previous.start_line,
                first_end: previous.end_line,
                second_start: current.start_line,
                second_end: current.end_line,
            });
        }
    }

    let mut buffer = contents.to_owned();
    pending.sort_by_key(|edit| edit.start_byte);
    for edit in pending.iter().rev() {
        buffer.replace_range(edit.start_byte..edit.end_byte, edit.new_text);
    }
    Ok(buffer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineSpan {
    content_start: usize,
    content_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingEdit<'a> {
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
    new_text: &'a str,
}

fn validate_edit<'a>(
    path: &Path,
    contents: &str,
    lines: &[LineSpan],
    index: usize,
    edit: &'a ExactTextEdit,
) -> Result<PendingEdit<'a>, WorkspaceFsError> {
    let edit_number = index.saturating_add(1);
    let start_line =
        usize::try_from(edit.start_line).map_err(|_error| WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        })?;
    let end_line =
        usize::try_from(edit.end_line).map_err(|_error| WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        })?;
    if start_line == 0 || end_line < start_line || end_line > lines.len() {
        return Err(WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        });
    }
    let first = lines.get(start_line.saturating_sub(1)).ok_or_else(|| {
        WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        }
    })?;
    let last = lines.get(end_line.saturating_sub(1)).ok_or_else(|| {
        WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        }
    })?;
    let actual = contents
        .get(first.content_start..last.content_end)
        .ok_or_else(|| WorkspaceFsError::InvalidTextBoundary {
            path: path.to_path_buf(),
            edit_number,
        })?;
    if actual != edit.old_text {
        return Err(WorkspaceFsError::OldTextMismatch {
            path: path.to_path_buf(),
            edit_number,
            expected: edit.old_text.clone(),
            actual: actual.to_owned(),
        });
    }
    Ok(PendingEdit {
        start_line,
        end_line,
        start_byte: first.content_start,
        end_byte: last.content_end,
        new_text: &edit.new_text,
    })
}

fn index_lines(contents: &str) -> Vec<LineSpan> {
    if contents.is_empty() {
        return vec![LineSpan {
            content_start: 0,
            content_end: 0,
        }];
    }
    let bytes = contents.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let content_end = if index > start && bytes.get(index.saturating_sub(1)) == Some(&b'\r')
            {
                index.saturating_sub(1)
            } else {
                index
            };
            lines.push(LineSpan {
                content_start: start,
                content_end,
            });
            start = index.saturating_add(1);
        }
    }
    if start < bytes.len() {
        lines.push(LineSpan {
            content_start: start,
            content_end: bytes.len(),
        });
    }
    lines
}

enum WriteMode {
    Create,
    Replace { permissions: fs::Permissions },
}

fn atomic_write(path: &Path, contents: &[u8], mode: WriteMode) -> Result<(), WorkspaceFsError> {
    let parent = path
        .parent()
        .ok_or_else(|| WorkspaceFsError::MissingParent(path.to_path_buf()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| WorkspaceFsError::MissingFileName(path.to_path_buf()))?
        .to_string_lossy();
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", Ulid::new()));
    let result = (|| {
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|source| WorkspaceFsError::Write {
                path: temp_path.clone(),
                source,
            })?;
        if let WriteMode::Replace { permissions } = &mode {
            temp.set_permissions(permissions.clone())
                .map_err(|source| WorkspaceFsError::Write {
                    path: temp_path.clone(),
                    source,
                })?;
        }
        temp.write_all(contents)
            .and_then(|()| temp.flush())
            .and_then(|()| temp.sync_all())
            .map_err(|source| WorkspaceFsError::Write {
                path: temp_path.clone(),
                source,
            })?;
        drop(temp);

        match mode {
            WriteMode::Create => rename_without_replacement(&temp_path, path)?,
            WriteMode::Replace { .. } => {
                fs::rename(&temp_path, path).map_err(|source| WorkspaceFsError::Write {
                    path: path.to_path_buf(),
                    source,
                })?;
            }
        }
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
fn rename_without_replacement(source: &Path, destination: &Path) -> Result<(), WorkspaceFsError> {
    if destination.exists() {
        return Err(WorkspaceFsError::AlreadyExists(destination.to_path_buf()));
    }
    fs::rename(source, destination).map_err(|source| WorkspaceFsError::Write {
        path: destination.to_path_buf(),
        source,
    })
}

fn sync_directory(path: &Path) -> Result<(), WorkspaceFsError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| WorkspaceFsError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceFsError {
    #[error("path has no file name: {0}")]
    MissingFileName(PathBuf),
    #[error("path has no parent directory: {0}")]
    MissingParent(PathBuf),
    #[error("unable to canonicalize {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unable to inspect {path}: {source}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("path is not a file: {0}")]
    NotFile(PathBuf),
    #[error("path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("file is not readable UTF-8 text at {path}: {source}")]
    ReadText {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("file already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("at least one edit is required")]
    NoEdits,
    #[error("invalid line range {start_line}-{end_line} for edit {edit_number} in {path}")]
    InvalidLineRange {
        path: PathBuf,
        edit_number: usize,
        start_line: u32,
        end_line: u32,
    },
    #[error("invalid UTF-8 boundary for edit {edit_number} in {path}")]
    InvalidTextBoundary { path: PathBuf, edit_number: usize },
    #[error(
        "old text mismatch for edit {edit_number} in {path}: expected {expected:?}, found {actual:?}"
    )]
    OldTextMismatch {
        path: PathBuf,
        edit_number: usize,
        expected: String,
        actual: String,
    },
    #[error(
        "edit ranges overlap in {path}: {first_start}-{first_end} and {second_start}-{second_end}"
    )]
    OverlappingEdits {
        path: PathBuf,
        first_start: usize,
        first_end: usize,
        second_start: usize,
        second_end: usize,
    },
    #[error("unable to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "filesystem unit tests use direct fixture and output assertions"
)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("kraai-workspace-fs-{name}-{}", Ulid::new()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn exact_edits_are_validated_before_any_replacement() {
        let edits = [
            ExactTextEdit {
                start_line: 1,
                end_line: 1,
                old_text: String::from("alpha"),
                new_text: String::from("one"),
            },
            ExactTextEdit {
                start_line: 2,
                end_line: 2,
                old_text: String::from("wrong"),
                new_text: String::from("two"),
            },
        ];
        let error = apply_exact_edits(Path::new("file"), "alpha\nbeta\n", &edits).unwrap_err();
        assert!(matches!(error, WorkspaceFsError::OldTextMismatch { .. }));
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
                old_text: String::from("beta"),
                new_text: String::from("gamma"),
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
        assert!(matches!(error, WorkspaceFsError::Write { .. }));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        let _ = fs::remove_dir_all(directory);
    }
}
