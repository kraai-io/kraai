#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::Serialize;
use tool_core::{ToolContext, ToolOutput, TypedTool, assess_read_path, resolve_tool_path};
use toon_schema::toon_tool;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolStateDelta};

const OPENED_FILES_NAMESPACE: &str = "opened_files";
const OPEN_OPERATION: &str = "open";

#[derive(Clone, Copy)]
pub struct OpenFileTool;

toon_tool! {
    name: "open_file",
    description: "Open a file for ongoing context injection in future turns without returning its full contents in chat history",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct OpenFileToolArgs {
            #[toon_schema(description = "File path to keep open for future turns")]
            path: String,
        }
    },
    root: OpenFileToolArgs,
    examples: [
        { path: "/path/to/file.txt" },
        { path: "src/lib.rs" },
    ]
}

#[derive(Serialize)]
struct OpenFileToolOutput {
    success: bool,
    path: String,
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
        let mut assessment = assess_read_path(
            &ctx.global_config.workspace_dir,
            &args.path,
            "Opens workspace file",
            "Opens file outside workspace",
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
            operation: String::from(OPEN_OPERATION),
            payload: serde_json::json!({ "path": resolved.path().display().to_string() }),
        }]
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolOutput {
        let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, &args.path);
        let path = resolved.path();

        if !path.exists() {
            return ToolOutput::error(format!("file does not exist: {}", path.display()));
        }
        if path.is_dir() {
            return ToolOutput::error(format!("path is a directory: {}", path.display()));
        }

        match std::fs::read_to_string(path) {
            Ok(_) => ToolOutput::success(OpenFileToolOutput {
                success: true,
                path: path.display().to_string(),
            }),
            Err(error) => {
                ToolOutput::error(format!("unable to read file {}: {}", path.display(), error))
            }
        }
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!("Open file for future context: {}", args.path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use tool_core::{ToolContext, ToolOutput, TypedTool};
    use types::{RiskLevel, ToolCallGlobalConfig};

    use super::{OpenFileTool, OpenFileToolArgs};

    fn tool_config(workspace_dir: &Path) -> ToolCallGlobalConfig {
        ToolCallGlobalConfig {
            workspace_dir: workspace_dir.to_path_buf(),
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
        let ctx = ToolContext {
            global_config: &config,
        };
        let args = OpenFileToolArgs {
            path: String::from("notes.txt"),
        };

        let assessment = tool.assess(&args, &ctx);
        assert_eq!(assessment.risk, RiskLevel::ReadOnlyWorkspace);

        let output = tool.call(args.clone(), &ctx).await;
        match output {
            ToolOutput::Success { data } => {
                let expected_path = workspace_dir.join("notes.txt").display().to_string();
                assert_eq!(data["path"].as_str(), Some(expected_path.as_str()));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        let deltas = tool.successful_tool_state_deltas(&args, &ctx);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].operation, "open");

        cleanup_temp_dir(&workspace_dir);
    }
}
