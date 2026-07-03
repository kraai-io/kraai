#![forbid(unsafe_code)]

use async_trait::async_trait;
use kraai_tool_core::{ToolCallResult, ToolContext, TypedTool, assess_read_path, read_text_file};
use kraai_toon_schema::toon_tool;
use kraai_types::{ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolStateDelta};
use serde::Serialize;

const OPENED_FILES_NAMESPACE: &str = "opened_files";
const OPEN_OPERATION: &str = "open";

#[derive(Clone, Copy)]
pub struct OpenFileTool;

toon_tool! {
    name: "open_files",
    description: "Open one or more files for ongoing context injection in future turns.\nOpened files are freshly read from disk before every turn and are authoritative.\nKeep files open while actively reasoning from them; close them only when they are no longer needed.",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct OpenFileToolArgs {
            #[toon_schema(description = "File paths to keep open for future turns")]
            paths: Vec<String>,
        }
    },
    root: OpenFileToolArgs,
    examples: [
        { paths: ["/path/to/file.txt"] },
        { paths: ["src/lib.rs", "src/main.rs"] },
    ]
}

#[derive(Serialize)]
struct OpenFileToolOutput {
    success: bool,
    paths: Vec<String>,
}

#[async_trait]
impl TypedTool for OpenFileTool {
    type Args = OpenFileToolArgs;

    fn name(&self) -> &'static str {
        OpenFileToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        OpenFileToolArgs::toon_schema()
    }

    fn assess(&self, args: &Self::Args, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let mut reasons = Vec::with_capacity(args.paths.len());
        let mut risk = RiskLevel::ReadOnlyWorkspace;
        for path in &args.paths {
            let assessment = assess_read_path(
                &ctx.global_config.workspace_dir,
                path,
                "Opens workspace file",
                "Opens file outside workspace",
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
            let read = match read_text_file(&ctx.global_config.workspace_dir, path) {
                Ok(read) => read,
                Err(error) => return ToolCallResult::error(error),
            };
            let path = read.path().display().to_string();
            paths.push(path.clone());
            deltas.push(ToolStateDelta {
                namespace: String::from(OPENED_FILES_NAMESPACE),
                operation: String::from(OPEN_OPERATION),
                payload: serde_json::json!({ "path": path }),
            });
        }

        ToolCallResult::success_with_deltas(
            OpenFileToolOutput {
                success: true,
                paths,
            },
            deltas,
        )
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Open files for future context: {}", args.paths.join(", "))
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests use direct assertions for tool output fixtures"
)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use kraai_tool_core::{ToolContext, ToolOutput, TypedTool};
    use kraai_types::{RiskLevel, ToolCallGlobalConfig, ToolStateSnapshot};

    use super::{OpenFileTool, OpenFileToolArgs};

    fn tool_config(workspace_dir: &Path) -> ToolCallGlobalConfig {
        ToolCallGlobalConfig::new(workspace_dir.to_path_buf())
    }

    fn tool_context<'a>(
        config: &'a ToolCallGlobalConfig,
        snapshot: &'a ToolStateSnapshot,
    ) -> ToolContext<'a> {
        ToolContext {
            global_config: config,
            tool_state_snapshot: snapshot,
        }
    }

    fn make_temp_dir(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agent-tool-open-file-{test_name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup_temp_dir(path: &PathBuf) {
        let _ = fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn opens_workspace_file_and_emits_delta() {
        let workspace_dir = make_temp_dir("opens_workspace_file_and_emits_delta");
        fs::write(workspace_dir.join("notes.txt"), "alpha").expect("write file");

        let tool = OpenFileTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let ctx = tool_context(&config, &snapshot);
        let args = OpenFileToolArgs {
            paths: vec![String::from("notes.txt")],
        };

        let assessment = tool.assess(&args, &ctx);
        assert_eq!(assessment.risk, RiskLevel::ReadOnlyWorkspace);

        let output = tool.call(args.clone(), &ctx).await;
        match output.output {
            ToolOutput::Success { data } => {
                let expected_path = workspace_dir.join("notes.txt").display().to_string();
                assert_eq!(data["paths"][0].as_str(), Some(expected_path.as_str()));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        assert_eq!(output.tool_state_deltas.len(), 1);
        assert_eq!(output.tool_state_deltas[0].operation, "open");

        cleanup_temp_dir(&workspace_dir);
    }
}
