use std::path::PathBuf;

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

#[derive(Debug, thiserror::Error)]
pub enum ScopedReadError {
    #[error("path no longer exists: {0}")]
    NotFound(PathBuf),
    #[error("path resolves outside its authorized root: {0}")]
    OutsideRoot(PathBuf),
    #[error("path is not a regular file: {0}")]
    NotFile(PathBuf),
    #[error("unable to open authorized root {path}: {source}")]
    OpenRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unable to open {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unable to inspect {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("file is not readable UTF-8 text at {path}: {source}")]
    ReadText {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("scoped pinned-file reads are unsupported on this platform: {0}")]
    UnsupportedPlatform(PathBuf),
}
