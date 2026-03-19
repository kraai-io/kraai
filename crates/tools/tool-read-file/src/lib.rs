#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::Serialize;
use tool_core::{
    ToolCallResult, ToolContext, TypedTool, file_read_refresh_delta, format_text_with_line_numbers,
    read_text_file, resolve_tool_path,
};
use toon_schema::toon_tool;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment};

#[derive(Clone, Copy)]
pub struct ReadFileTool;

toon_tool! {
    name: "read_files",
    description: "Read files from the filesystem and return their contents with line numbers",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct ReadFileToolArgs {
            #[toon_schema(description = "List of file paths to read", min = 1)]
            files: Vec<String>,
        }
    },
    root: ReadFileToolArgs,
    examples: [
        { files: ["/path/to/file.txt"] },
        { files: ["/path/to/file.txt", "/path/to/another/file.md"] },
        { files: ["/path/to/file.txt", "/path/to/another/file.md", "path/to/a/file/in/current/workspace"] },
    ]
}

#[derive(Serialize)]
struct ReadFileToolOutput {
    files: Vec<String>,
}

#[async_trait]
impl TypedTool for ReadFileTool {
    type Args = ReadFileToolArgs;

    fn name(&self) -> &'static str {
        ReadFileToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        ReadFileToolArgs::toon_schema()
    }

    fn assess(&self, args: &Self::Args, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let mut reasons = Vec::new();
        let mut risk = RiskLevel::ReadOnlyWorkspace;

        for file in &args.files {
            let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, file);
            if resolved.is_within_workspace() {
                reasons.push(format!(
                    "Reads workspace file {}",
                    resolved.path().display()
                ));
            } else {
                risk = RiskLevel::ReadOnlyOutsideWorkspace;
                reasons.push(format!(
                    "Reads file outside workspace {}",
                    resolved.path().display()
                ));
            }
        }

        ToolCallAssessment {
            risk,
            policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
            reasons,
        }
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolCallResult {
        let mut files_out = Vec::with_capacity(args.files.len());
        let mut tool_state_deltas = Vec::with_capacity(args.files.len());

        for file in args.files {
            let read = match read_text_file(&ctx.global_config.workspace_dir, &file) {
                Ok(read) => read,
                Err(error) => return ToolCallResult::error(error),
            };
            files_out.push(format_text_with_line_numbers(read.contents()));
            tool_state_deltas.push(file_read_refresh_delta(read.path(), read.sha256()));
        }

        let out = ReadFileToolOutput { files: files_out };
        ToolCallResult::success_with_deltas(out, tool_state_deltas)
    }

    fn describe(&self, args: &Self::Args) -> String {
        let count = args.files.len();
        let files_str = args.files.join(", ");
        format!("Read {} file(s): {}", count, files_str)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use tool_core::{FILE_READS_NAMESPACE, ToolContext, ToolOutput, TypedTool};
    use types::{ExecutionPolicy, RiskLevel, ToolCallGlobalConfig, ToolStateSnapshot};

    use super::{ReadFileTool, ReadFileToolArgs};

    fn tool_config(workspace_dir: &Path) -> ToolCallGlobalConfig {
        ToolCallGlobalConfig {
            workspace_dir: workspace_dir.to_path_buf(),
        }
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
            "agent-tool-read-file-{test_name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup_temp_dir(path: &PathBuf) {
        let _ = fs::remove_dir_all(path);
    }

    fn read_args(files: &[impl AsRef<str>]) -> ReadFileToolArgs {
        ReadFileToolArgs {
            files: files.iter().map(|file| file.as_ref().to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn reads_workspace_relative_paths_with_one_based_line_numbers() {
        let workspace_dir =
            make_temp_dir("reads_workspace_relative_paths_with_one_based_line_numbers");
        fs::write(workspace_dir.join("notes.txt"), "alpha\nbeta\n").expect("write file");

        let tool = ReadFileTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(read_args(&["notes.txt"]), &tool_context(&config, &snapshot))
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let files = data["files"].as_array().expect("files array");
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].as_str(), Some("1|alpha\n2|beta"));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }
        assert_eq!(output.tool_state_deltas.len(), 1);
        assert_eq!(output.tool_state_deltas[0].namespace, FILE_READS_NAMESPACE);

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn reads_multiple_files_in_order() {
        let workspace_dir = make_temp_dir("reads_multiple_files_in_order");
        fs::write(workspace_dir.join("a.txt"), "first").expect("write first file");
        fs::write(workspace_dir.join("b.txt"), "second").expect("write second file");

        let tool = ReadFileTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                read_args(&["a.txt", "b.txt"]),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let files = data["files"].as_array().expect("files array");
                let contents = files
                    .iter()
                    .map(|value| value.as_str().expect("file output"))
                    .collect::<Vec<_>>();
                assert_eq!(contents, vec!["1|first", "1|second"]);
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }
        assert_eq!(output.tool_state_deltas.len(), 2);

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn reads_paths_relative_to_workspace_root_even_for_parent_traversal() {
        let workspace_dir =
            make_temp_dir("reads_paths_relative_to_workspace_root_even_for_parent_traversal");
        let outside_dir = make_temp_dir("read_file_outside_target");
        fs::write(outside_dir.join("outside.txt"), "external").expect("write outside file");

        let tool = ReadFileTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let outside_path = format!(
            "../{}/outside.txt",
            outside_dir
                .file_name()
                .expect("outside dir name")
                .to_string_lossy()
        );
        let output = tool
            .call(
                read_args(&[outside_path]),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                let files = data["files"].as_array().expect("files array");
                assert_eq!(files[0].as_str(), Some("1|external"));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
        cleanup_temp_dir(&outside_dir);
    }

    #[test]
    fn assess_marks_workspace_paths_as_read_only() {
        let workspace_dir = make_temp_dir("assess_marks_workspace_paths_as_read_only");
        let tool = ReadFileTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let assessment = tool.assess(
            &read_args(&["notes.txt"]),
            &tool_context(&config, &snapshot),
        );

        assert_eq!(assessment.risk, RiskLevel::ReadOnlyWorkspace);
        assert_eq!(
            assessment.policy,
            ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace)
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assess_marks_parent_traversal_as_outside_workspace() {
        let workspace_dir = make_temp_dir("assess_marks_parent_traversal_as_outside_workspace");
        let outside_dir = make_temp_dir("read_file_assess_outside_target");
        let tool = ReadFileTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let relative_path = format!(
            "../{}",
            outside_dir
                .file_name()
                .expect("outside dir name")
                .to_string_lossy()
        );
        let assessment = tool.assess(
            &read_args(&[relative_path]),
            &tool_context(&config, &snapshot),
        );

        assert_eq!(assessment.risk, RiskLevel::ReadOnlyOutsideWorkspace);

        cleanup_temp_dir(&workspace_dir);
        cleanup_temp_dir(&outside_dir);
    }

    #[tokio::test]
    async fn missing_file_error_matches_open_file_behavior() {
        let workspace_dir = make_temp_dir("missing_file_error_matches_open_file_behavior");
        let tool = ReadFileTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let output = tool
            .call(
                read_args(&["missing.txt"]),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Error { message } => {
                assert_eq!(
                    message,
                    format!(
                        "file does not exist: {}",
                        workspace_dir.join("missing.txt").display()
                    )
                );
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }
}
