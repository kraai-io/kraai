use std::io::Read;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tool_core::{Tool, ToolContext, ToolOutput, resolve_tool_path};
use toon_schema::ToonSchema;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment};

pub struct ReadFileTool;

#[derive(Deserialize, ToonSchema, Serialize)]
#[toon_schema(
    name = "read_files",
    description = "Read files from the filesystem and return their contents with line numbers"
)]
struct ReadFileToolArgs {
    #[toon_schema(
        description = "List of file paths to read",
        example = "[\"/path/to/file.txt\", \"/path/to/another/file.md\"]",
        min = 1
    )]
    files: Vec<String>,
}

#[derive(Serialize)]
struct ReadFileToolOutput {
    files: Vec<String>,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        ReadFileToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        ReadFileToolArgs::toon_schema()
    }

    fn assess(&self, args: &serde_json::Value, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let parsed: ReadFileToolArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolCallAssessment {
                    risk: RiskLevel::ReadOnlyOutsideWorkspace,
                    policy: ExecutionPolicy::AlwaysAsk,
                    reasons: vec![format!("Unable to validate read_files arguments: {error}")],
                };
            }
        };

        let mut reasons = Vec::new();
        let mut risk = RiskLevel::ReadOnlyWorkspace;

        for file in &parsed.files {
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

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> ToolOutput {
        let args: ReadFileToolArgs = match serde_json::from_value(args) {
            Ok(x) => x,
            Err(e) => {
                return ToolOutput::error(format!("args error: {}", e));
            }
        };
        let mut files_out = vec![];
        for f in args.files {
            let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, &f);
            let mut file = match std::fs::File::open(resolved.path()) {
                Ok(f) => f,
                Err(e) => {
                    return ToolOutput::error(format!(
                        "unable to open file {}: {}",
                        resolved.path().display(),
                        e
                    ));
                }
            };
            let mut str = String::new();
            match file.read_to_string(&mut str) {
                Ok(_) => {}
                Err(e) => {
                    return ToolOutput::error(format!(
                        "unable to read file {}: {}",
                        resolved.path().display(),
                        e
                    ));
                }
            }
            files_out.push(format_file_with_line_numbers(&str));
        }
        let out = ReadFileToolOutput { files: files_out };
        ToolOutput::success(out)
    }

    async fn describe(&self, args: serde_json::Value) -> String {
        let args: ReadFileToolArgs =
            serde_json::from_value(args).unwrap_or(ReadFileToolArgs { files: vec![] });
        let count = args.files.len();
        let files_str = args.files.join(", ");
        format!("Read {} file(s): {}", count, files_str)
    }
}

fn format_file_with_line_numbers(contents: &str) -> String {
    contents
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{}|{}", index + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tool_core::{Tool, ToolContext, ToolOutput};
    use types::{ExecutionPolicy, RiskLevel, ToolCallGlobalConfig};

    use super::ReadFileTool;

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
            "agent-tool-read-file-{test_name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup_temp_dir(path: &PathBuf) {
        let _ = fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn reads_workspace_relative_paths_with_one_based_line_numbers() {
        let workspace_dir =
            make_temp_dir("reads_workspace_relative_paths_with_one_based_line_numbers");
        fs::write(workspace_dir.join("notes.txt"), "alpha\nbeta\n").expect("write file");

        let tool = ReadFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                json!({ "files": ["notes.txt"] }),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Success { data } => {
                let files = data["files"].as_array().expect("files array");
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].as_str(), Some("1|alpha\n2|beta"));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn reads_multiple_files_in_order() {
        let workspace_dir = make_temp_dir("reads_multiple_files_in_order");
        fs::write(workspace_dir.join("a.txt"), "first").expect("write first file");
        fs::write(workspace_dir.join("b.txt"), "second").expect("write second file");

        let tool = ReadFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                json!({ "files": ["a.txt", "b.txt"] }),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
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
        let output = tool
            .call(
                json!({ "files": [format!("../{}/outside.txt", outside_dir.file_name().expect("outside dir name").to_string_lossy())] }),
                &ToolContext { global_config: &config },
            )
            .await;

        match output {
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
        let assessment = tool.assess(
            &json!({ "files": ["notes.txt"] }),
            &ToolContext {
                global_config: &config,
            },
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
        let assessment = tool.assess(
            &json!({ "files": [format!("../{}", outside_dir.file_name().expect("outside dir name").to_string_lossy())] }),
            &ToolContext { global_config: &config },
        );

        assert_eq!(assessment.risk, RiskLevel::ReadOnlyOutsideWorkspace);

        cleanup_temp_dir(&workspace_dir);
        cleanup_temp_dir(&outside_dir);
    }
}
