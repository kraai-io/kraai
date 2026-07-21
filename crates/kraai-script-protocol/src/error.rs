#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    MalformedStartTag(String),
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
            Self::MissingTimeout => write!(f, "tool_call requires a timeout attribute"),
            Self::DuplicateAttribute(name) => write!(f, "duplicate tool_call attribute '{name}'"),
            Self::UnknownAttribute(name) => write!(f, "unknown tool_call attribute '{name}'"),
            Self::InvalidTimeout(message) => write!(f, "invalid tool_call timeout: {message}"),
            Self::InvalidPermissions(message) => {
                write!(f, "invalid tool_call permissions: {message}")
            }
            Self::EmptyScript => write!(f, "tool_call script is empty"),
            Self::IncompleteScript => write!(f, "tool_call stream ended before </tool_call>"),
        }
    }
}

impl std::error::Error for ProtocolError {}
