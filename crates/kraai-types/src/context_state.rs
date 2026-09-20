use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{CommandInvocationId, ScriptExecutionId};

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_context_event_preserves_persisted_schema() {
        let event = ContextStateEvent {
            id: String::from("event"),
            source: ContextStateEventSource::Command {
                execution_id: ScriptExecutionId::new("execution"),
                sequence: 3,
                invocation_id: CommandInvocationId::new("invocation"),
                command_id: String::from("kraai-open-files"),
            },
            mutations: vec![
                ContextStateMutation::PinFile {
                    path: PathBuf::from("file.rs"),
                    scope: PinnedFileScope::Workspace {
                        root: PathBuf::from("workspace"),
                    },
                },
                ContextStateMutation::PinFile {
                    path: PathBuf::from("host.rs"),
                    scope: PinnedFileScope::Host,
                },
                ContextStateMutation::UnpinFile {
                    path: PathBuf::from("removed.rs"),
                    reason: Some(String::from("gone")),
                },
            ],
        };
        let expected = r#"{"id":"event","source":{"kind":"command","execution_id":"execution","sequence":3,"invocation_id":"invocation","command_id":"kraai-open-files"},"mutations":[{"kind":"pin-file","path":"file.rs","scope":{"kind":"workspace","root":"workspace"}},{"kind":"pin-file","path":"host.rs","scope":{"kind":"host"}},{"kind":"unpin-file","path":"removed.rs","reason":"gone"}]}"#;
        assert_eq!(
            serde_json::to_string(&event).as_deref().ok(),
            Some(expected)
        );
        assert_eq!(
            serde_json::from_str::<ContextStateEvent>(expected).ok(),
            Some(event)
        );
    }

    #[test]
    fn runtime_context_event_preserves_optional_reason_and_unknown_fields() {
        let original = r#"{"id":"event","source":{"kind":"runtime","component":"refresh","extra":true},"mutations":[{"kind":"unpin-file","path":"file.rs"}],"extra":true}"#;
        let expected = ContextStateEvent {
            id: String::from("event"),
            source: ContextStateEventSource::Runtime {
                component: String::from("refresh"),
            },
            mutations: vec![ContextStateMutation::UnpinFile {
                path: PathBuf::from("file.rs"),
                reason: None,
            }],
        };
        assert_eq!(
            serde_json::from_str::<ContextStateEvent>(original).ok(),
            Some(expected.clone())
        );
        assert_eq!(
            serde_json::to_string(&expected).as_deref().ok(),
            Some(
                r#"{"id":"event","source":{"kind":"runtime","component":"refresh"},"mutations":[{"kind":"unpin-file","path":"file.rs","reason":null}]}"#
            ),
        );
    }
}
