use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use kraai_execution_types::{CommandInvocationId, ScriptExecutionId};

use crate::MessageId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum PinnedFileScope {
    Workspace { root: PathBuf },
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ContextStateMutation {
    PinFile {
        path: PathBuf,
        scope: PinnedFileScope,
    },
    UnpinFile {
        path: PathBuf,
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ContextStateEventSource {
    Command {
        execution_id: ScriptExecutionId,
        sequence: u64,
        invocation_id: CommandInvocationId,
        command_id: String,
    },
    Runtime {
        component: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextStateEvent {
    pub id: String,
    pub source: ContextStateEventSource,
    pub mutations: Vec<ContextStateMutation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
}

#[cfg(test)]
mod tests;
