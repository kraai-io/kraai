use std::path::PathBuf;

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
