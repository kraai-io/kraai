use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tool_core::{Tool, ToolContext, ToolOutput, assess_read_only_path, resolve_tool_path};
use toon_schema::ToonSchema;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment};

pub struct ListFilesTool;

#[derive(Deserialize, ToonSchema, Serialize)]
#[toon_schema(
    name = "list_files",
    description = "List files in a directory like ls. Returns a shallow directory listing and includes hidden files."
)]
struct ListFilesToolArgs {
    #[toon_schema(
        description = "Directory path to list",
        example = "\"/path/to/directory\""
    )]
    path: String,
}

#[derive(Serialize)]
struct ListFilesToolOutput {
    path: String,
    entries: Vec<ListFilesEntry>,
}

#[derive(Serialize)]
struct ListFilesEntry {
    name: String,
    path: String,
    is_dir: bool,
}

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &'static str {
        ListFilesToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        ListFilesToolArgs::toon_schema()
    }

    fn assess(&self, args: &serde_json::Value, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let parsed: ListFilesToolArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolCallAssessment {
                    risk: RiskLevel::OutsideWorkspace,
                    policy: ExecutionPolicy::AlwaysAsk,
                    reasons: vec![format!("Unable to validate list_files arguments: {error}")],
                };
            }
        };

        assess_read_only_path(
            &ctx.global_config.workspace_dir,
            &parsed.path,
            "Lists workspace directory",
            "Lists directory outside workspace",
        )
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> ToolOutput {
        let args: ListFilesToolArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return ToolOutput::error(format!("args error: {error}")),
        };

        let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, &args.path);
        let metadata = match std::fs::metadata(resolved.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                return ToolOutput::error(format!(
                    "unable to access directory {}: {}",
                    resolved.path().display(),
                    error
                ));
            }
        };

        if !metadata.is_dir() {
            return ToolOutput::error(format!(
                "path is not a directory: {}",
                resolved.path().display()
            ));
        }

        let entries = match read_entries(resolved.path()) {
            Ok(entries) => entries,
            Err(error) => {
                return ToolOutput::error(format!(
                    "unable to list directory {}: {}",
                    resolved.path().display(),
                    error
                ));
            }
        };

        ToolOutput::success(ListFilesToolOutput {
            path: resolved.path().display().to_string(),
            entries,
        })
    }

    async fn describe(&self, args: serde_json::Value) -> String {
        let args: ListFilesToolArgs = serde_json::from_value(args).unwrap_or(ListFilesToolArgs {
            path: String::new(),
        });
        format!("List files in {}", args.path)
    }
}

fn read_entries(dir: &Path) -> std::io::Result<Vec<ListFilesEntry>> {
    let mut entries = std::fs::read_dir(dir)?
        .map(|entry| {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;
            Ok(ListFilesEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: path.display().to_string(),
                is_dir: metadata.is_dir(),
            })
        })
        .collect::<std::io::Result<Vec<_>>>()?;

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tool_core::{Tool, ToolContext, ToolOutput};
    use types::{ExecutionPolicy, RiskLevel, ToolCallGlobalConfig};

    use super::ListFilesTool;

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
            "agent-tool-list-files-{test_name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup_temp_dir(path: &PathBuf) {
        let _ = fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn lists_hidden_and_visible_entries_sorted() {
        let workspace_dir = make_temp_dir("lists_hidden_and_visible_entries_sorted");
        fs::write(workspace_dir.join("z-last.txt"), "z").expect("write visible file");
        fs::write(workspace_dir.join(".hidden"), "hidden").expect("write hidden file");
        fs::create_dir(workspace_dir.join("folder")).expect("create folder");

        let tool = ListFilesTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                json!({ "path": "." }),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Success { data } => {
                let entries = data["entries"].as_array().expect("entries array");
                let names = entries
                    .iter()
                    .map(|entry| entry["name"].as_str().expect("entry name"))
                    .collect::<Vec<_>>();
                assert_eq!(names, vec![".hidden", "folder", "z-last.txt"]);
                assert_eq!(
                    data["path"].as_str(),
                    Some(workspace_dir.to_string_lossy().as_ref())
                );
                assert_eq!(entries[1]["is_dir"].as_bool(), Some(true));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn listing_is_shallow_only() {
        let workspace_dir = make_temp_dir("listing_is_shallow_only");
        let nested_dir = workspace_dir.join("nested");
        fs::create_dir(&nested_dir).expect("create nested dir");
        fs::write(nested_dir.join("deep.txt"), "deep").expect("write deep file");

        let tool = ListFilesTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                json!({ "path": "." }),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Success { data } => {
                let entries = data["entries"].as_array().expect("entries array");
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0]["name"].as_str(), Some("nested"));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn returns_error_for_missing_directory() {
        let workspace_dir = make_temp_dir("returns_error_for_missing_directory");
        let tool = ListFilesTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                json!({ "path": "missing" }),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("unable to access directory"));
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn returns_error_for_file_path() {
        let workspace_dir = make_temp_dir("returns_error_for_file_path");
        fs::write(workspace_dir.join("file.txt"), "file").expect("write file");
        let tool = ListFilesTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                json!({ "path": "file.txt" }),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("path is not a directory"));
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assess_marks_workspace_path_as_read_only() {
        let workspace_dir = make_temp_dir("assess_marks_workspace_path_as_read_only");
        let tool = ListFilesTool;
        let config = tool_config(&workspace_dir);
        let assessment = tool.assess(
            &json!({ "path": "." }),
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
    fn assess_marks_outside_workspace_path_as_outside() {
        let workspace_dir = make_temp_dir("assess_marks_outside_workspace_path_as_outside");
        let outside_dir = make_temp_dir("assess_outside_target");
        let tool = ListFilesTool;
        let config = tool_config(&workspace_dir);
        let assessment = tool.assess(
            &json!({ "path": outside_dir.to_string_lossy() }),
            &ToolContext {
                global_config: &config,
            },
        );

        assert_eq!(assessment.risk, RiskLevel::OutsideWorkspace);

        cleanup_temp_dir(&workspace_dir);
        cleanup_temp_dir(&outside_dir);
    }

    #[test]
    fn assess_marks_relative_parent_path_as_outside() {
        let workspace_dir = make_temp_dir("assess_marks_relative_parent_path_as_outside");
        let outside_dir = make_temp_dir("assess_relative_outside_target");
        let relative_path = format!(
            "../{}",
            outside_dir
                .file_name()
                .expect("outside dir name")
                .to_string_lossy()
        );
        let tool = ListFilesTool;
        let config = tool_config(&workspace_dir);
        let assessment = tool.assess(
            &json!({ "path": relative_path }),
            &ToolContext {
                global_config: &config,
            },
        );

        assert_eq!(assessment.risk, RiskLevel::OutsideWorkspace);

        cleanup_temp_dir(&workspace_dir);
        cleanup_temp_dir(&outside_dir);
    }
}
