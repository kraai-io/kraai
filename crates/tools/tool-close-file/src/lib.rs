#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::Serialize;
use tool_core::{ToolContext, ToolOutput, TypedTool, assess_read_path, resolve_tool_path};
use toon_schema::toon_tool;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolStateDelta};

const OPENED_FILES_NAMESPACE: &str = "opened_files";
const CLOSE_OPERATION: &str = "close";

#[derive(Clone, Copy)]
pub struct CloseFileTool;

toon_tool! {
    name: "close_file",
    description: "Close a previously opened file so it stops being injected into future turns",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct CloseFileToolArgs {
            #[toon_schema(description = "File path to remove from future injected context")]
            path: String,
        }
    },
    root: CloseFileToolArgs,
    examples: [
        { path: "/path/to/file.txt" },
        { path: "src/lib.rs" },
    ]
}

#[derive(Serialize)]
struct CloseFileToolOutput {
    success: bool,
    path: String,
}

#[async_trait]
impl TypedTool for CloseFileTool {
    type Args = CloseFileToolArgs;

    fn name(&self) -> &'static str {
        CloseFileToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        CloseFileToolArgs::toon_schema()
    }

    fn assess(&self, args: &Self::Args, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let mut assessment = assess_read_path(
            &ctx.global_config.workspace_dir,
            &args.path,
            "Closes workspace file",
            "Closes file outside workspace",
        );
        assessment.policy = ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace);
        assessment
    }

    fn successful_tool_state_deltas(
        &self,
        args: &Self::Args,
        ctx: &ToolContext<'_>,
    ) -> Vec<ToolStateDelta> {
        let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, &args.path);
        vec![ToolStateDelta {
            namespace: String::from(OPENED_FILES_NAMESPACE),
            operation: String::from(CLOSE_OPERATION),
            payload: serde_json::json!({ "path": resolved.path().display().to_string() }),
        }]
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolOutput {
        let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, &args.path);
        ToolOutput::success(CloseFileToolOutput {
            success: true,
            path: resolved.path().display().to_string(),
        })
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Close file from future context: {}", args.path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tool_core::{ToolContext, TypedTool};
    use types::{RiskLevel, ToolCallGlobalConfig};

    use super::{CloseFileTool, CloseFileToolArgs};

    fn tool_config(workspace_dir: &Path) -> ToolCallGlobalConfig {
        ToolCallGlobalConfig {
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    #[test]
    fn closes_missing_file_and_emits_delta() {
        let tool = CloseFileTool;
        let workspace_dir = PathBuf::from("/tmp/workspace");
        let config = tool_config(&workspace_dir);
        let ctx = ToolContext {
            global_config: &config,
        };
        let args = CloseFileToolArgs {
            path: String::from("missing.txt"),
        };

        let assessment = tool.assess(&args, &ctx);
        assert_eq!(assessment.risk, RiskLevel::ReadOnlyWorkspace);

        let deltas = tool.successful_tool_state_deltas(&args, &ctx);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].operation, "close");
    }
}
