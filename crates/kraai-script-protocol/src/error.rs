#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    MalformedStartTag(String),
    MalformedMetadata(String),
    MissingTimeout,
    DuplicateAttribute(String),
    UnknownAttribute(String),
    InvalidTimeout(String),
    InvalidPermissions(String),
    EmptyScript,
    IncompleteScript,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedStartTag(message) => write!(f, "malformed tool_call tag: {message}"),
            Self::MalformedMetadata(message) => write!(f, "malformed script metadata: {message}"),
            Self::MissingTimeout => write!(f, "script metadata requires a timeout field"),
            Self::DuplicateAttribute(name) => write!(f, "duplicate script metadata field '{name}'"),
            Self::UnknownAttribute(name) => write!(f, "unknown script metadata field '{name}'"),
            Self::InvalidTimeout(message) => write!(f, "invalid script timeout: {message}"),
            Self::InvalidPermissions(message) => {
                write!(f, "invalid script permissions: {message}")
            }
            Self::EmptyScript => write!(f, "tool_call script is empty"),
            Self::IncompleteScript => write!(f, "tool_call stream ended before </tool_call>"),
        }
    }
}

impl std::error::Error for ProtocolError {}
