#[derive(Debug, PartialEq, Eq)]
pub enum SandboxError {
    ExecutableMustBeAbsolute,
    ExecutableNotVisible(String),
    InvalidTimeout,
    WorkspaceReadRequired,
    MissingWorkspace(String),
    InvalidRuntimeRoot(String),
    PrivateTemp(String),
    SandboxUnavailable(String),
    Spawn { executable: String, message: String },
    Wait(String),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExecutableMustBeAbsolute => write!(f, "executable path must be absolute"),
            Self::ExecutableNotVisible(path) => write!(
                f,
                "executable '{path}' is outside the workspace and configured runtime roots"
            ),
            Self::InvalidTimeout => write!(f, "timeout must be greater than zero"),
            Self::WorkspaceReadRequired => write!(
                f,
                "sandboxed execution requires the workspace-read capability"
            ),
            Self::MissingWorkspace(message) => write!(f, "invalid workspace root: {message}"),
            Self::InvalidRuntimeRoot(message) => write!(f, "invalid runtime root: {message}"),
            Self::PrivateTemp(message) => write!(f, "private temporary directory error: {message}"),
            Self::SandboxUnavailable(message) => write!(f, "sandbox unavailable: {message}"),
            Self::Spawn {
                executable,
                message,
            } => write!(f, "unable to spawn '{}': {message}", executable),
            Self::Wait(message) => write!(f, "unable to wait for process: {message}"),
        }
    }
}

impl std::error::Error for SandboxError {}
