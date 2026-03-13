#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use async_trait::async_trait;
use serde::Serialize;
use tool_core::{ToolContext, ToolOutput, TypedTool, assess_write_path, resolve_tool_path};
use toon_schema::toon_tool;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment};

#[derive(Clone, Copy)]
pub struct EditFileTool;

toon_tool! {
    name: "edit_file",
    description: "Apply one or more deterministic exact-text replacements to a single file",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        struct EditOperation {
            #[toon_schema(description = "Exact existing text to replace")]
            old_text: String,
            #[toon_schema(description = "Replacement text")]
            new_text: String,
        }

        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct EditFileToolArgs {
            #[toon_schema(description = "File path to edit or create")]
            path: String,

            #[serde(default)]
            #[toon_schema(description = "When true, create a new file instead of editing an existing one")]
            create: bool,

            #[toon_schema(description = "Edit operations to apply", min = 1)]
            edits: Vec<EditOperation>,
        }
    },
    root: EditFileToolArgs,
    examples: [
        {
            path: "src/lib.rs",
            create: false,
            edits: [
                { old_text: "old", new_text: "new" }
            ]
        },
        {
            path: "src/new_file.rs",
            create: true,
            edits: [
                { old_text: "", new_text: "pub fn hello() {\\n    println!(\\\"hello\\\");\\n}\\n" }
            ]
        }
    ]
}

#[derive(Serialize)]
struct EditFileToolSuccess {
    success: bool,
}

#[async_trait]
impl TypedTool for EditFileTool {
    type Args = EditFileToolArgs;

    fn name(&self) -> &'static str {
        EditFileToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        EditFileToolArgs::toon_schema()
    }

    fn assess(&self, args: &Self::Args, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let mut assessment = assess_write_path(
            &ctx.global_config.workspace_dir,
            &args.path,
            if args.create {
                "Creates workspace file"
            } else {
                "Edits workspace file"
            },
            if args.create {
                "Creates file outside workspace"
            } else {
                "Edits file outside workspace"
            },
        );
        if assessment.risk == RiskLevel::UndoableWorkspaceWrite {
            assessment.policy = ExecutionPolicy::AutonomousUpTo(RiskLevel::UndoableWorkspaceWrite);
        }
        assessment
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolOutput {
        let resolved = resolve_tool_path(&ctx.global_config.workspace_dir, &args.path);
        let result = if args.create {
            create_file(resolved.path(), &args.edits)
        } else {
            edit_file(resolved.path(), &args.edits)
        };

        match result {
            Ok(()) => ToolOutput::success(EditFileToolSuccess { success: true }),
            Err(error) => ToolOutput::error(error),
        }
    }

    fn describe(&self, args: &Self::Args) -> String {
        if args.create {
            format!(
                "Create file {} with {} edit(s)",
                args.path,
                args.edits.len()
            )
        } else {
            format!(
                "Edit file {} with {} replacement(s)",
                args.path,
                args.edits.len()
            )
        }
    }
}

fn create_file(path: &Path, edits: &[EditOperation]) -> Result<(), String> {
    if path.exists() {
        return Err(format!("file already exists: {}", path.display()));
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("parent directory does not exist: {}", path.display()))?;
    if !parent.exists() {
        return Err(format!(
            "parent directory does not exist: {}",
            parent.display()
        ));
    }
    if !parent.is_dir() {
        return Err(format!(
            "parent path is not a directory: {}",
            parent.display()
        ));
    }

    if edits.len() != 1 {
        return Err(String::from(
            "create=true requires exactly one edit operation",
        ));
    }
    if !edits[0].old_text.is_empty() {
        return Err(String::from(
            "create=true requires the only edit to have old_text set to an empty string",
        ));
    }

    fs::write(path, &edits[0].new_text)
        .map_err(|error| format!("unable to create file {}: {}", path.display(), error))
}

fn edit_file(path: &Path, edits: &[EditOperation]) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("file does not exist: {}", path.display()));
    }
    if path.is_dir() {
        return Err(format!("path is a directory: {}", path.display()));
    }

    let original = fs::read_to_string(path)
        .map_err(|error| format!("unable to read file {}: {}", path.display(), error))?;
    let updated = apply_edits(&original, edits)?;

    fs::write(path, updated)
        .map_err(|error| format!("unable to write file {}: {}", path.display(), error))
}

fn apply_edits(contents: &str, edits: &[EditOperation]) -> Result<String, String> {
    let mut buffer = contents.to_string();

    for edit in edits {
        let match_index = find_unique_match(&buffer, &edit.old_text)?;
        let end = match_index + edit.old_text.len();
        buffer.replace_range(match_index..end, &edit.new_text);
    }

    Ok(buffer)
}

fn find_unique_match(haystack: &str, needle: &str) -> Result<usize, String> {
    if needle.is_empty() {
        return Err(String::from(
            "old_text must match exactly once, but an empty string is ambiguous",
        ));
    }

    let mut matches = haystack.match_indices(needle);
    let first = matches
        .next()
        .map(|(index, _)| index)
        .ok_or_else(|| format!("old_text not found: {:?}", needle))?;

    if matches.next().is_some() {
        return Err(format!(
            "old_text matched multiple times and is ambiguous: {:?}",
            needle
        ));
    }

    Ok(first)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tool_core::{ToolContext, ToolOutput, TypedTool};
    use toon_format::decode_default;
    use types::{ExecutionPolicy, RiskLevel, ToolCallGlobalConfig};

    use super::{EditFileTool, EditFileToolArgs, EditOperation};

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
            "agent-tool-edit-file-{test_name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup_temp_dir(path: &PathBuf) {
        let _ = fs::remove_dir_all(path);
    }

    fn edit_args(
        path: impl Into<String>,
        create: bool,
        edits: &[(&str, &str)],
    ) -> EditFileToolArgs {
        EditFileToolArgs {
            path: path.into(),
            create,
            edits: edits
                .iter()
                .map(|(old_text, new_text)| EditOperation {
                    old_text: (*old_text).to_string(),
                    new_text: (*new_text).to_string(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn edits_workspace_file_with_one_exact_replacement() {
        let workspace_dir = make_temp_dir("edits_workspace_file_with_one_exact_replacement");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", false, &[("beta", "gamma")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Success { data } => assert_eq!(data, json!({ "success": true })),
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }
        assert_eq!(
            fs::read_to_string(path).expect("read file"),
            "alpha\ngamma\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn applies_multiple_edits_in_order_when_each_match_is_unique() {
        let workspace_dir =
            make_temp_dir("applies_multiple_edits_in_order_when_each_match_is_unique");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", false, &[("alpha", "one"), ("gamma", "three")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Success { data } => assert_eq!(data, json!({ "success": true })),
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }
        assert_eq!(
            fs::read_to_string(path).expect("read file"),
            "one\nbeta\nthree\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn fails_when_old_text_does_not_exist() {
        let workspace_dir = make_temp_dir("fails_when_old_text_does_not_exist");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", false, &[("missing", "gamma")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => assert!(message.contains("old_text not found")),
            ToolOutput::Success { .. } => panic!("expected error"),
        }
        assert_eq!(
            fs::read_to_string(path).expect("read file"),
            "alpha\nbeta\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn fails_when_old_text_matches_multiple_times() {
        let workspace_dir = make_temp_dir("fails_when_old_text_matches_multiple_times");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "dup\ndup\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", false, &[("dup", "unique")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => assert!(message.contains("matched multiple times")),
            ToolOutput::Success { .. } => panic!("expected error"),
        }
        assert_eq!(fs::read_to_string(path).expect("read file"), "dup\ndup\n");

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn validates_all_edits_before_writing_anything() {
        let workspace_dir = make_temp_dir("validates_all_edits_before_writing_anything");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", false, &[("alpha", "one"), ("missing", "two")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => assert!(message.contains("old_text not found")),
            ToolOutput::Success { .. } => panic!("expected error"),
        }
        assert_eq!(
            fs::read_to_string(path).expect("read file"),
            "alpha\nbeta\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn create_mode_creates_missing_file() {
        let workspace_dir = make_temp_dir("create_mode_creates_missing_file");
        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("created.txt", true, &[("", "hello\n")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Success { data } => assert_eq!(data, json!({ "success": true })),
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }
        assert_eq!(
            fs::read_to_string(workspace_dir.join("created.txt")).expect("read file"),
            "hello\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn create_mode_fails_if_file_already_exists() {
        let workspace_dir = make_temp_dir("create_mode_fails_if_file_already_exists");
        fs::write(workspace_dir.join("created.txt"), "existing").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("created.txt", true, &[("", "hello\n")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => assert!(message.contains("file already exists")),
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn create_mode_fails_if_parent_directory_is_missing() {
        let workspace_dir = make_temp_dir("create_mode_fails_if_parent_directory_is_missing");
        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("missing/created.txt", true, &[("", "hello\n")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("parent directory does not exist"))
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assess_marks_workspace_edits_as_undoable_workspace_writes() {
        let workspace_dir =
            make_temp_dir("assess_marks_workspace_edits_as_undoable_workspace_writes");
        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let assessment = tool.assess(
            &edit_args("notes.txt", false, &[("a", "b")]),
            &ToolContext {
                global_config: &config,
            },
        );

        assert_eq!(assessment.risk, RiskLevel::UndoableWorkspaceWrite);
        assert_eq!(
            assessment.policy,
            ExecutionPolicy::AutonomousUpTo(RiskLevel::UndoableWorkspaceWrite)
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn path_traversal_outside_workspace_returns_outside_write_assessment() {
        let workspace_dir =
            make_temp_dir("path_traversal_outside_workspace_returns_outside_write_assessment");
        let outside_dir = make_temp_dir("edit_file_outside_target");
        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let assessment = tool.assess(
            &edit_args(
                format!(
                    "../{}/notes.txt",
                    outside_dir
                        .file_name()
                        .expect("outside dir name")
                        .to_string_lossy()
                ),
                false,
                &[("a", "b")],
            ),
            &ToolContext {
                global_config: &config,
            },
        );

        assert_eq!(assessment.risk, RiskLevel::WriteOutsideWorkspace);
        assert_eq!(assessment.policy, ExecutionPolicy::AlwaysAsk);

        cleanup_temp_dir(&workspace_dir);
        cleanup_temp_dir(&outside_dir);
    }

    #[tokio::test]
    async fn describe_produces_readable_summary() {
        let tool = EditFileTool;
        let description = tool.describe(&edit_args("src/lib.rs", false, &[("old", "new")]));

        assert_eq!(description, "Edit file src/lib.rs with 1 replacement(s)");
    }

    #[test]
    fn native_toon_edit_arguments_validate_successfully() {
        let args: serde_json::Value = decode_default(
            "path: notes.txt\ncreate: false\nedits[1]{old_text,new_text}:\n  beta,gamma",
        )
        .expect("decode native toon edit args");
        let mut manager = tool_core::ToolManager::new();
        manager.register_tool(EditFileTool);
        manager
            .prepare_tool(&types::ToolId::new("edit_file"), args)
            .expect("validate native edit args");
    }
}
