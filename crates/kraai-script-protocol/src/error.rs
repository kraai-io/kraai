#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    MalformedMetadata(String),
    MissingTimeout,
    DuplicateAttribute(String),
    UnknownAttribute(String),
    InvalidTimeout(String),
    InvalidPermissions(String),
    EmptyScript,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedMetadata(message) => write!(f, "malformed script metadata: {message}"),
            Self::MissingTimeout => write!(f, "script metadata requires a timeout field"),
            Self::DuplicateAttribute(name) => write!(f, "duplicate script metadata field '{name}'"),
            Self::UnknownAttribute(name) => write!(f, "unknown script metadata field '{name}'"),
            Self::InvalidTimeout(message) => write!(f, "invalid script timeout: {message}"),
            Self::InvalidPermissions(message) => {
                write!(f, "invalid script permissions: {message}")
            }
            Self::EmptyScript => write!(f, "tool_call script is empty"),
        }
    }
}

impl std::error::Error for ProtocolError {}
