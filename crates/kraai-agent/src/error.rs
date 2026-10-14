use kraai_execution_types::DomainError;

/// A failure of an agent operation: either the request was wrong for the current state, or the
/// service failed.
#[derive(Debug)]
pub enum AgentError {
    Domain(DomainError),
    Internal(color_eyre::Report),
}

pub type AgentResult<T> = Result<T, AgentError>;

impl AgentError {
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::Domain(DomainError::invalid_argument(message))
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::Domain(DomainError::not_found(message))
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Domain(DomainError::conflict(message))
    }

    /// The domain failure, if this is one.
    pub fn domain(&self) -> Option<&DomainError> {
        match self {
            Self::Domain(error) => Some(error),
            Self::Internal(_) => None,
        }
    }
}

impl From<DomainError> for AgentError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

/// Persistence and MCP report domain failures, such as a session leased by another runtime,
/// inside their `eyre` reports. This is the one place those are told apart from service
/// failures.
impl From<color_eyre::Report> for AgentError {
    fn from(report: color_eyre::Report) -> Self {
        let report = match report.downcast::<Self>() {
            Ok(error) => return error,
            Err(report) => report,
        };
        match report.downcast::<DomainError>() {
            Ok(error) => Self::Domain(error),
            Err(report) => Self::Internal(report),
        }
    }
}

impl From<std::io::Error> for AgentError {
    fn from(error: std::io::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl From<tokio::task::JoinError> for AgentError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Internal(error.into())
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(error) => error.fmt(formatter),
            Self::Internal(report) => write!(formatter, "{report:#}"),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Domain(error) => Some(error),
            Self::Internal(report) => Some(report.as_ref()),
        }
    }
}

impl From<kraai_provider_core::ProviderError> for AgentError {
    fn from(error: kraai_provider_core::ProviderError) -> Self {
        Self::Internal(error.into())
    }
}
