use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
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
    HostUnavailable,
    RuntimeError,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptExecutionOutcome {
    pub status: ScriptExecutionStatus,
    pub exit_code: Option<i32>,
}

impl ScriptExecutionOutcome {
    pub fn succeeded(self) -> bool {
        self.status == ScriptExecutionStatus::Completed && self.exit_code == Some(0)
    }
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
            Self::HostUnavailable => "host-unavailable",
            Self::RuntimeError => "runtime-error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptExecutionPhase {
    Prepared,
    AwaitingApproval,
    Running,
    Finished,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptOutputStream {
    Stdout,
    Stderr,
}

/// A sandbox rule that no capability besides `no-sandbox` lifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxHint {
    ProtectedEntry,
    ProtectedEntryUndone,
    HostSocket,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptRewrites {
    #[serde(default, rename = "rewrites")]
    pub bash: Vec<String>,
    #[serde(default)]
    pub merged_calls: usize,
    #[serde(default)]
    pub dropped_calls: usize,
}

impl ScriptRewrites {
    pub fn is_empty(&self) -> bool {
        self.bash.is_empty() && self.merged_calls == 0 && self.dropped_calls == 0
    }
}
