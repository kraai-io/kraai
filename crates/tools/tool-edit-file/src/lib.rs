#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use async_trait::async_trait;
use serde::Serialize;
use tool_core::{Tool, ToolContext, ToolOutput, assess_write_path, resolve_tool_path};
use toon_schema::toon_tool;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment};

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

        #[derive(serde::Deserialize, serde::Serialize)]
        struct EditFileToolArgs {
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
impl Tool for EditFileTool {
    fn name(&self) -> &'static str {
        EditFileToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        EditFileToolArgs::toon_schema()
    }

    fn validate(&self, args: &serde_json::Value) -> Result<(), String> {
        serde_json::from_value::<EditFileToolArgs>(args.clone())
            .map(|_| ())
            .map_err(|error| format!("Unable to validate edit_file arguments: {error}"))
    }

    fn assess(&self, args: &serde_json::Value, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let parsed: EditFileToolArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolCallAssessment {
                    risk: RiskLevel::WriteOutsideWorkspace,
                    policy: ExecutionPolicy::AlwaysAsk,
                    reasons: vec![format!("Unable to validate edit_file arguments: {error}")],
                };
            }
        };

        let mut assessment = assess_write_path(
            &ctx.global_config.workspace_dir,
            &parsed.path,
            if parsed.create {
                "Creates workspace file"
            } else {
                "Edits workspace file"
            },
            if parsed.create {
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

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> ToolOutput {
        let args: EditFileToolArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return ToolOutput::error(format!("args error: {error}")),
        };

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

    async fn describe(&self, args: serde_json::Value) -> String {
        let args: EditFileToolArgs = serde_json::from_value(args).unwrap_or(EditFileToolArgs {
            path: String::new(),
            create: false,
            edits: Vec::new(),
        });

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
    use tool_core::{Tool, ToolContext, ToolOutput};
    use toon_format::decode_default;
    use types::{ExecutionPolicy, RiskLevel, ToolCallGlobalConfig};

    use super::EditFileTool;

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

    #[tokio::test]
    async fn edits_workspace_file_with_one_exact_replacement() {
        let workspace_dir = make_temp_dir("edits_workspace_file_with_one_exact_replacement");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                json!({
                    "path": "notes.txt",
                    "edits": [{ "old_text": "beta", "new_text": "gamma" }]
                }),
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
                json!({
                    "path": "notes.txt",
                    "edits": [
                        { "old_text": "alpha", "new_text": "one" },
                        { "old_text": "gamma", "new_text": "three" }
                    ]
                }),
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
                json!({
                    "path": "notes.txt",
                    "edits": [{ "old_text": "missing", "new_text": "gamma" }]
                }),
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
                json!({
                    "path": "notes.txt",
                    "edits": [{ "old_text": "dup", "new_text": "unique" }]
                }),
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
                json!({
                    "path": "notes.txt",
                    "edits": [
                        { "old_text": "alpha", "new_text": "one" },
                        { "old_text": "missing", "new_text": "two" }
                    ]
                }),
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
                json!({
                    "path": "created.txt",
                    "create": true,
                    "edits": [{ "old_text": "", "new_text": "hello\n" }]
                }),
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
                json!({
                    "path": "created.txt",
                    "create": true,
                    "edits": [{ "old_text": "", "new_text": "hello\n" }]
                }),
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
                json!({
                    "path": "missing/created.txt",
                    "create": true,
                    "edits": [{ "old_text": "", "new_text": "hello\n" }]
                }),
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
            &json!({
                "path": "notes.txt",
                "edits": [{ "old_text": "a", "new_text": "b" }]
            }),
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
            &json!({
                "path": format!(
                    "../{}/notes.txt",
                    outside_dir.file_name().expect("outside dir name").to_string_lossy()
                ),
                "edits": [{ "old_text": "a", "new_text": "b" }]
            }),
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
        let description = tool
            .describe(json!({
                "path": "src/lib.rs",
                "edits": [{ "old_text": "old", "new_text": "new" }]
            }))
            .await;

        assert_eq!(description, "Edit file src/lib.rs with 1 replacement(s)");
    }

    #[test]
    fn native_toon_edit_arguments_validate_successfully() {
        let tool = EditFileTool;
        let args: serde_json::Value = decode_default(
            "path: notes.txt\ncreate: false\nedits[1]{old_text,new_text}:\n  beta,gamma",
        )
        .expect("decode native toon edit args");

        tool.validate(&args).expect("validate native edit args");
    }
}
