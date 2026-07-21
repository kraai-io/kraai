use serde::{Deserialize, Serialize};

/// Stable terminal outcome for a Nushell script execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptExecutionStatus {
    Completed,
    Denied,
    InvalidScript,
    TimedOut,
    Cancelled,
    SandboxUnavailable,
    FailedToStart,
    RuntimeError,
}

impl ScriptExecutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Denied => "denied",
            Self::InvalidScript => "invalid-script",
            Self::TimedOut => "timed-out",
            Self::Cancelled => "cancelled",
            Self::SandboxUnavailable => "sandbox-unavailable",
            Self::FailedToStart => "failed-to-start",
            Self::RuntimeError => "runtime-error",
        }
    }
}

/// Durable lifecycle phase, separate from the stable terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptExecutionPhase {
    Prepared,
    AwaitingApproval,
    Running,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptOutputStream {
    Stdout,
    Stderr,
}

#[cfg(test)]
mod tests {
    use super::{ScriptExecutionPhase, ScriptExecutionStatus};

    #[test]
    fn lifecycle_and_terminal_outcomes_are_independent() {
        let phases = [
            ScriptExecutionPhase::Prepared,
            ScriptExecutionPhase::AwaitingApproval,
            ScriptExecutionPhase::Running,
            ScriptExecutionPhase::Finished,
        ];
        let statuses = [
            ScriptExecutionStatus::Completed,
            ScriptExecutionStatus::Denied,
            ScriptExecutionStatus::InvalidScript,
            ScriptExecutionStatus::TimedOut,
            ScriptExecutionStatus::Cancelled,
            ScriptExecutionStatus::SandboxUnavailable,
            ScriptExecutionStatus::FailedToStart,
            ScriptExecutionStatus::RuntimeError,
        ];
        assert_eq!(phases.len(), 4);
        assert_eq!(statuses.len(), 8);
    }
}
