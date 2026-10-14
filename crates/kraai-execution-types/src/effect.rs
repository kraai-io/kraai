use serde::{Deserialize, Serialize};

use crate::CommandInvocationId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextStateDelta {
    pub namespace: String,
    pub operation: String,
    pub payload: serde_json::Value,
}

impl ContextStateDelta {
    pub fn opened_file_path(&self) -> Option<&str> {
        (self.namespace == OpenedFilesOperation::NAMESPACE)
            .then(|| self.payload.get("path").and_then(serde_json::Value::as_str))
            .flatten()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenedFilesOperation {
    Open,
    Close,
}

impl OpenedFilesOperation {
    pub const NAMESPACE: &'static str = "opened_files";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Close => "close",
        }
    }

    pub fn parse(operation: &str) -> Option<Self> {
        match operation {
            "open" => Some(Self::Open),
            "close" => Some(Self::Close),
            _ => None,
        }
    }

    pub fn into_delta(self, path: impl Into<String>) -> ContextStateDelta {
        ContextStateDelta {
            namespace: String::from(Self::NAMESPACE),
            operation: String::from(self.as_str()),
            payload: serde_json::Value::Object(serde_json::Map::from_iter([(
                String::from("path"),
                serde_json::Value::String(path.into()),
            )])),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEffectRequest {
    pub sequence: u64,
    pub invocation_id: CommandInvocationId,
    pub command_id: String,
    pub deltas: Vec<ContextStateDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEffectAck {
    pub invocation_id: CommandInvocationId,
    pub error: Option<String>,
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "wire contract tests assert after fallible serialization"
)]
mod tests {
    use super::*;

    #[test]
    fn opened_file_effects_keep_the_generic_wire_contract() -> Result<(), serde_json::Error> {
        for (operation, wire_operation) in [
            (OpenedFilesOperation::Open, "open"),
            (OpenedFilesOperation::Close, "close"),
        ] {
            let delta = operation.into_delta("/workspace/🦀.rs");
            let expected = serde_json::json!({
                "namespace": "opened_files",
                "operation": wire_operation,
                "payload": { "path": "/workspace/🦀.rs" },
            });
            let encoded = serde_json::to_value(&delta)?;
            assert_eq!(encoded, expected);
            assert_eq!(delta.opened_file_path(), Some("/workspace/🦀.rs"));
            assert_eq!(
                OpenedFilesOperation::parse(&delta.operation),
                Some(operation)
            );
        }
        Ok(())
    }

    #[test]
    fn generic_effects_preserve_unknown_operations_and_payload_fields()
    -> Result<(), serde_json::Error> {
        let value = serde_json::json!({
            "namespace": "opened_files",
            "operation": "future-operation",
            "payload": { "path": "/workspace/file", "extra": true },
            "extra": "ignored",
        });
        let delta: ContextStateDelta = serde_json::from_value(value)?;
        assert_eq!(delta.opened_file_path(), Some("/workspace/file"));
        assert_eq!(OpenedFilesOperation::parse(&delta.operation), None);
        assert_eq!(
            delta.payload.get("extra"),
            Some(&serde_json::Value::Bool(true))
        );
        Ok(())
    }
}
