pub mod toon_parser;

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolCallGlobalConfig, ToolId};

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    ToolNotFound(ToolId),
    #[error("Failed to serialize tool output: {0}")]
    OutputSerialization(#[from] serde_json::Error),
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum ToolOutput {
    Success {
        #[serde(flatten)]
        data: serde_json::Value,
    },
    Error {
        message: String,
    },
}

impl ToolOutput {
    pub fn error(message: String) -> Self {
        Self::Error { message }
    }

    pub fn success<D: Serialize>(data: D) -> Self {
        match serde_json::to_value(data) {
            Ok(data) => Self::Success { data },
            Err(error) => Self::error(format!("failed to serialize tool output: {error}")),
        }
    }
}

pub struct ToolContext<'a> {
    pub global_config: &'a ToolCallGlobalConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToolPath {
    path: PathBuf,
    within_workspace: bool,
}

impl ResolvedToolPath {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_within_workspace(&self) -> bool {
        self.within_workspace
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn schema(&self) -> &'static str;

    fn assess(&self, _args: &serde_json::Value, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::WriteOutsideWorkspace,
            policy: ExecutionPolicy::AlwaysAsk,
            reasons: vec![String::from(
                "Tool does not define a custom autonomy policy",
            )],
        }
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> ToolOutput;

    async fn describe(&self, args: serde_json::Value) -> String {
        format!(
            "{}: {}",
            self.name(),
            serde_json::to_string(&args).unwrap_or_default()
        )
    }
}

#[derive(Default, Clone)]
pub struct ToolManager {
    tools: BTreeMap<ToolId, Arc<dyn Tool>>,
}

impl ToolManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_tool(&mut self, tool: impl Tool + 'static) {
        let id = ToolId::new(tool.name());
        self.tools.insert(id, Arc::new(tool));
    }

    pub fn has_tool(&self, id: &ToolId) -> bool {
        self.tools.contains_key(id)
    }

    pub fn get_tool(&self, id: &ToolId) -> Option<Arc<dyn Tool>> {
        self.tools.get(id).cloned()
    }

    pub fn list_tools(&self) -> Vec<ToolId> {
        self.tools.keys().cloned().collect()
    }

    pub fn generate_system_prompt(&self) -> String {
        self.tools
            .values()
            .map(|t| t.schema())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub async fn call_tool(
        &self,
        id: &ToolId,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .tools
            .get(id)
            .ok_or_else(|| ToolError::ToolNotFound(id.clone()))?;
        Ok(tool.call(args, ctx).await)
    }

    pub fn assess_tool(
        &self,
        id: &ToolId,
        args: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolCallAssessment, ToolError> {
        let tool = self
            .tools
            .get(id)
            .ok_or_else(|| ToolError::ToolNotFound(id.clone()))?;
        Ok(tool.assess(args, ctx))
    }

    pub async fn describe_tool(
        &self,
        id: &ToolId,
        args: serde_json::Value,
    ) -> Result<String, ToolError> {
        let tool = self
            .tools
            .get(id)
            .ok_or_else(|| ToolError::ToolNotFound(id.clone()))?;
        Ok(tool.describe(args).await)
    }
}

pub fn normalize_tool_path(workspace_root: &Path, raw_path: &str) -> PathBuf {
    let path = Path::new(raw_path);
    let is_absolute = path.is_absolute();
    let base = if is_absolute {
        PathBuf::new()
    } else {
        workspace_root.to_path_buf()
    };

    let mut normalized = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.parent().is_some() {
                    normalized.pop();
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    if is_absolute && !normalized.is_absolute() {
        normalized = Path::new("/").join(normalized);
    }

    normalized
}

pub fn resolve_tool_path(workspace_root: &Path, raw_path: &str) -> ResolvedToolPath {
    let path = normalize_tool_path(workspace_root, raw_path);
    let within_workspace = path.starts_with(workspace_root);
    ResolvedToolPath {
        path,
        within_workspace,
    }
}

pub fn assess_read_path(
    workspace_root: &Path,
    raw_path: &str,
    workspace_reason_prefix: &str,
    outside_reason_prefix: &str,
) -> ToolCallAssessment {
    let resolved = resolve_tool_path(workspace_root, raw_path);
    let reason = if resolved.is_within_workspace() {
        format!("{} {}", workspace_reason_prefix, resolved.path().display())
    } else {
        format!("{} {}", outside_reason_prefix, resolved.path().display())
    };

    ToolCallAssessment {
        risk: if resolved.is_within_workspace() {
            RiskLevel::ReadOnlyWorkspace
        } else {
            RiskLevel::ReadOnlyOutsideWorkspace
        },
        policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
        reasons: vec![reason],
    }
}

pub fn assess_write_path(
    workspace_root: &Path,
    raw_path: &str,
    workspace_reason_prefix: &str,
    outside_reason_prefix: &str,
) -> ToolCallAssessment {
    let resolved = resolve_tool_path(workspace_root, raw_path);
    let reason = if resolved.is_within_workspace() {
        format!("{} {}", workspace_reason_prefix, resolved.path().display())
    } else {
        format!("{} {}", outside_reason_prefix, resolved.path().display())
    };

    ToolCallAssessment {
        risk: if resolved.is_within_workspace() {
            RiskLevel::UndoableWorkspaceWrite
        } else {
            RiskLevel::WriteOutsideWorkspace
        },
        policy: ExecutionPolicy::AlwaysAsk,
        reasons: vec![reason],
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde::ser::{Error as _, Serialize, Serializer};
    use types::{ExecutionPolicy, RiskLevel};

    use super::{ToolOutput, assess_read_path, assess_write_path, resolve_tool_path};

    struct FailingSerialize;

    impl Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(S::Error::custom("intentional failure"))
        }
    }

    #[test]
    fn tool_output_success_falls_back_to_error_on_serialize_failure() {
        let output = ToolOutput::success(FailingSerialize);

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("failed to serialize tool output"));
                assert!(message.contains("intentional failure"));
            }
            ToolOutput::Success { .. } => panic!("expected tool serialization failure"),
        }
    }

    #[test]
    fn resolve_tool_path_marks_parent_traversal_outside_workspace() {
        let workspace_root = Path::new("/tmp/workspace");
        let resolved = resolve_tool_path(workspace_root, "../elsewhere/file.txt");

        assert_eq!(resolved.path(), Path::new("/tmp/elsewhere/file.txt"));
        assert!(!resolved.is_within_workspace());
    }

    #[test]
    fn assess_read_path_uses_workspace_policy_for_inside_paths() {
        let workspace_root = Path::new("/tmp/workspace");
        let assessment = assess_read_path(
            workspace_root,
            "src/lib.rs",
            "Reads workspace file",
            "Reads file outside workspace",
        );

        assert_eq!(assessment.risk, RiskLevel::ReadOnlyWorkspace);
        assert_eq!(
            assessment.policy,
            ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace)
        );
        assert_eq!(
            assessment.reasons,
            vec![String::from(
                "Reads workspace file /tmp/workspace/src/lib.rs"
            )]
        );
    }

    #[test]
    fn assess_write_path_uses_write_risk_levels() {
        let workspace_root = Path::new("/tmp/workspace");

        let inside = assess_write_path(
            workspace_root,
            "src/lib.rs",
            "Edits workspace file",
            "Edits file outside workspace",
        );
        assert_eq!(inside.risk, RiskLevel::UndoableWorkspaceWrite);
        assert_eq!(inside.policy, ExecutionPolicy::AlwaysAsk);

        let outside = assess_write_path(
            workspace_root,
            "../elsewhere/file.txt",
            "Edits workspace file",
            "Edits file outside workspace",
        );
        assert_eq!(outside.risk, RiskLevel::WriteOutsideWorkspace);
        assert_eq!(outside.policy, ExecutionPolicy::AlwaysAsk);
    }
}
