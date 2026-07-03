#![forbid(unsafe_code)]

use async_trait::async_trait;
use kraai_tool_core::{
    ToolCallResult, ToolContext, TypedTool, assess_read_path, resolve_tool_path,
};
use kraai_toon_schema::toon_tool;
use kraai_types::{ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolStateDelta};
use serde::Serialize;

const OPENED_FILES_NAMESPACE: &str = "opened_files";
const CLOSE_OPERATION: &str = "close";

#[derive(Clone, Copy)]
pub struct CloseFileTool;

toon_tool! {
    name: "close_files",
    description: "Close one or more previously opened files so they stop being injected into future turns.\nUse this to clean up files that are no longer needed.\nDo NOT close a file while your current task, conclusion, edit, or review still depends on its contents.\nPrefer closing files after you have finished the reasoning/report that used them, not before.\nRule of thumb: if you would need to re-open the file to answer a follow-up about your current conclusion, keep it open.",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct CloseFileToolArgs {
            #[toon_schema(description = "File paths to remove from future injected context")]
            paths: Vec<String>,
        }
    },
    root: CloseFileToolArgs,
    examples: [
        { paths: ["/path/to/file.txt"] },
        { paths: ["src/lib.rs"] },
    ]
}

#[derive(Serialize)]
struct CloseFileToolOutput {
    success: bool,
    paths: Vec<String>,
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
        let mut reasons = Vec::with_capacity(args.paths.len());
        let mut risk = RiskLevel::ReadOnlyWorkspace;
        for path in &args.paths {
            let assessment = assess_read_path(
                &ctx.global_config.workspace_dir,
                path,
                "Closes workspace file",
                "Closes file outside workspace",
            );
            if assessment.risk > risk {
                risk = assessment.risk;
            }
            reasons.extend(assessment.reasons);
        }

        ToolCallAssessment {
            risk,
            policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
            reasons,
        }
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolCallResult {
        let mut paths = Vec::with_capacity(args.paths.len());
        let mut deltas = Vec::with_capacity(args.paths.len());
        for path in &args.paths {
            let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, path);
            let path = resolved.path().display().to_string();
            paths.push(path.clone());
            deltas.push(ToolStateDelta {
                namespace: String::from(OPENED_FILES_NAMESPACE),
                operation: String::from(CLOSE_OPERATION),
                payload: serde_json::json!({ "path": path }),
            });
        }

        ToolCallResult::success_with_deltas(
            CloseFileToolOutput {
                success: true,
                paths,
            },
            deltas,
        )
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Close files from future context: {}", args.paths.join(", "))
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "test inspects the single expected tool-state delta"
)]
mod tests {
    use std::path::{Path, PathBuf};

    use kraai_tool_core::{ToolContext, TypedTool};
    use kraai_types::{RiskLevel, ToolCallGlobalConfig, ToolStateSnapshot};

    use super::{CloseFileTool, CloseFileToolArgs};

    fn tool_config(workspace_dir: &Path) -> ToolCallGlobalConfig {
        ToolCallGlobalConfig::new(workspace_dir.to_path_buf())
    }

    #[tokio::test]
    async fn closes_missing_file_and_emits_delta() {
        let tool = CloseFileTool;
        let workspace_dir = PathBuf::from("/tmp/workspace");
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let ctx = ToolContext {
            global_config: &config,
            tool_state_snapshot: &snapshot,
        };
        let args = CloseFileToolArgs {
            paths: vec![String::from("missing.txt")],
        };

        let assessment = tool.assess(&args, &ctx);
        assert_eq!(assessment.risk, RiskLevel::ReadOnlyWorkspace);

        let output = tool.call(args, &ctx).await;
        assert_eq!(output.tool_state_deltas.len(), 1);
        assert_eq!(output.tool_state_deltas[0].operation, "close");
    }
}
