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
    description: "Apply one or more deterministic exact line-ranged replacements to a single file",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        struct EditOperation {
            #[toon_schema(description = "One-based inclusive start line for the exact snippet to replace")]
            start_line: u32,
            #[toon_schema(description = "One-based inclusive end line for the exact snippet to replace")]
            end_line: u32,
            #[toon_schema(description = "Exact existing text for the selected line range, joined with \\n and no trailing line terminator")]
            old_text: String,
            #[toon_schema(description = "Replacement text for the selected snippet; this text does not include line numbers")]
            new_text: String,
        }

        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct EditFileToolArgs {
            #[toon_schema(description = "File path to edit or create")]
            path: String,

            #[serde(default)]
            #[toon_schema(description = "When true, create a new file instead of editing an existing one")]
            create: bool,

            #[toon_schema(description = "Contents for create=true. Required when creating and forbidden for normal edits")]
            contents: Option<String>,

            #[toon_schema(description = "Edit operations for create=false. Each operation must declare the exact one-based inclusive line range being replaced")]
            edits: Option<Vec<EditOperation>>,
        }
    },
    root: EditFileToolArgs,
    examples: [
        {
            path: "src/lib.rs",
            create: false,
            edits: [
                { start_line: 10, end_line: 12, old_text: "fn old() {\\n    beta();\\n}", new_text: "fn new() {\\n    gamma();\\n}" }
            ]
        },
        {
            path: "src/new_file.rs",
            create: true,
            contents: "pub fn hello() {\\n    println!(\\\"hello\\\");\\n}\\n"
        }
    ]
}

#[derive(Serialize)]
struct EditFileToolSuccess {
    success: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineSpan {
    content_start: usize,
    content_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingEdit<'a> {
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
    new_text: &'a str,
}

enum ValidatedArgs<'a> {
    Create { contents: &'a str },
    Edit { edits: &'a [EditOperation] },
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
        let result = match validate_args(&args) {
            Ok(ValidatedArgs::Create { contents }) => create_file(resolved.path(), contents),
            Ok(ValidatedArgs::Edit { edits }) => edit_file(resolved.path(), edits),
            Err(error) => Err(error),
        };

        match result {
            Ok(()) => ToolOutput::success(EditFileToolSuccess { success: true }),
            Err(error) => ToolOutput::error(error),
        }
    }

    fn describe(&self, args: &Self::Args) -> String {
        if args.create {
            format!("Create file {}", args.path)
        } else {
            format!(
                "Edit file {} with {} replacement(s)",
                args.path,
                args.edits.as_ref().map_or(0, Vec::len)
            )
        }
    }
}

fn validate_args(args: &EditFileToolArgs) -> Result<ValidatedArgs<'_>, String> {
    if args.create {
        if args.edits.is_some() {
            return Err(String::from("create=true requires edits to be omitted"));
        }

        let contents = args
            .contents
            .as_deref()
            .ok_or_else(|| String::from("create=true requires contents"))?;
        return Ok(ValidatedArgs::Create { contents });
    }

    if args.contents.is_some() {
        return Err(String::from("create=false requires contents to be omitted"));
    }

    let edits = args
        .edits
        .as_deref()
        .ok_or_else(|| String::from("create=false requires edits"))?;
    if edits.is_empty() {
        return Err(String::from("create=false requires at least one edit"));
    }

    Ok(ValidatedArgs::Edit { edits })
}

fn create_file(path: &Path, contents: &str) -> Result<(), String> {
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

    fs::write(path, contents)
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
    let updated = apply_edits(path, &original, edits)?;

    fs::write(path, updated)
        .map_err(|error| format!("unable to write file {}: {}", path.display(), error))
}

fn apply_edits(path: &Path, contents: &str, edits: &[EditOperation]) -> Result<String, String> {
    let lines = index_lines(contents);
    let mut pending = Vec::with_capacity(edits.len());

    for (index, edit) in edits.iter().enumerate() {
        pending.push(validate_edit(path, contents, &lines, index, edit)?);
    }

    pending.sort_by_key(|edit| (edit.start_line, edit.end_line));
    for window in pending.windows(2) {
        let previous = window[0];
        let current = window[1];
        if current.start_line <= previous.end_line {
            return Err(format!(
                "edit_file {} edit ranges overlap: lines {}-{} and lines {}-{}",
                path.display(),
                previous.start_line,
                previous.end_line,
                current.start_line,
                current.end_line
            ));
        }
    }

    let mut buffer = contents.to_string();
    pending.sort_by_key(|edit| edit.start_byte);
    for edit in pending.iter().rev() {
        buffer.replace_range(edit.start_byte..edit.end_byte, edit.new_text);
    }

    Ok(buffer)
}

fn validate_edit<'a>(
    path: &Path,
    contents: &str,
    lines: &[LineSpan],
    index: usize,
    edit: &'a EditOperation,
) -> Result<PendingEdit<'a>, String> {
    let edit_index = index + 1;
    let start_line = usize::try_from(edit.start_line).map_err(|_| {
        format!(
            "edit_file {} edit {} has an invalid start_line {}",
            path.display(),
            edit_index,
            edit.start_line
        )
    })?;
    let end_line = usize::try_from(edit.end_line).map_err(|_| {
        format!(
            "edit_file {} edit {} has an invalid end_line {}",
            path.display(),
            edit_index,
            edit.end_line
        )
    })?;

    if start_line == 0 {
        return Err(format!(
            "edit_file {} edit {} has invalid line range {}-{}: line numbers are one-based",
            path.display(),
            edit_index,
            edit.start_line,
            edit.end_line
        ));
    }
    if end_line < start_line {
        return Err(format!(
            "edit_file {} edit {} has invalid line range {}-{}",
            path.display(),
            edit_index,
            edit.start_line,
            edit.end_line
        ));
    }

    if lines.is_empty() {
        if start_line != 1 || end_line != 1 {
            return Err(format!(
                "edit_file {} edit {} line range {}-{} is out of bounds for empty file",
                path.display(),
                edit_index,
                edit.start_line,
                edit.end_line
            ));
        }
        if !edit.old_text.is_empty() {
            return Err(format!(
                "edit_file {} edit {} lines 1-1 do not match old_text",
                path.display(),
                edit_index
            ));
        }
        return Ok(PendingEdit {
            start_line,
            end_line,
            start_byte: 0,
            end_byte: 0,
            new_text: &edit.new_text,
        });
    }

    if end_line > lines.len() {
        return Err(format!(
            "edit_file {} edit {} line range {}-{} is out of bounds for file with {} line(s)",
            path.display(),
            edit_index,
            edit.start_line,
            edit.end_line,
            lines.len()
        ));
    }

    let expected = render_logical_range(contents, lines, start_line, end_line);
    if edit.old_text != expected {
        return Err(format!(
            "edit_file {} edit {} lines {}-{} do not match old_text",
            path.display(),
            edit_index,
            edit.start_line,
            edit.end_line
        ));
    }

    let start_span = lines[start_line - 1];
    let end_span = lines[end_line - 1];
    Ok(PendingEdit {
        start_line,
        end_line,
        start_byte: start_span.content_start,
        end_byte: end_span.content_end,
        new_text: &edit.new_text,
    })
}

fn render_logical_range(
    contents: &str,
    lines: &[LineSpan],
    start_line: usize,
    end_line: usize,
) -> String {
    let mut rendered = String::new();

    for (index, line_number) in (start_line..=end_line).enumerate() {
        if index > 0 {
            rendered.push('\n');
        }
        let span = lines[line_number - 1];
        rendered.push_str(&contents[span.content_start..span.content_end]);
    }

    rendered
}

fn index_lines(contents: &str) -> Vec<LineSpan> {
    let bytes = contents.as_bytes();
    let mut lines = Vec::new();
    let mut line_start = 0usize;

    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }

        let content_end = if index > line_start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        lines.push(LineSpan {
            content_start: line_start,
            content_end,
        });
        line_start = index + 1;
    }

    if line_start < contents.len() {
        lines.push(LineSpan {
            content_start: line_start,
            content_end: contents.len(),
        });
    }

    lines
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

    fn edit_args(path: impl Into<String>, edits: &[(u32, u32, &str, &str)]) -> EditFileToolArgs {
        EditFileToolArgs {
            path: path.into(),
            create: false,
            contents: None,
            edits: Some(
                edits
                    .iter()
                    .map(|(start_line, end_line, old_text, new_text)| EditOperation {
                        start_line: *start_line,
                        end_line: *end_line,
                        old_text: (*old_text).to_string(),
                        new_text: (*new_text).to_string(),
                    })
                    .collect(),
            ),
        }
    }

    fn create_args(path: impl Into<String>, contents: impl Into<String>) -> EditFileToolArgs {
        EditFileToolArgs {
            path: path.into(),
            create: true,
            contents: Some(contents.into()),
            edits: None,
        }
    }

    fn invalid_args(
        path: impl Into<String>,
        create: bool,
        contents: Option<&str>,
        edits: Option<&[(u32, u32, &str, &str)]>,
    ) -> EditFileToolArgs {
        EditFileToolArgs {
            path: path.into(),
            create,
            contents: contents.map(ToString::to_string),
            edits: edits.map(|edits| {
                edits
                    .iter()
                    .map(|(start_line, end_line, old_text, new_text)| EditOperation {
                        start_line: *start_line,
                        end_line: *end_line,
                        old_text: (*old_text).to_string(),
                        new_text: (*new_text).to_string(),
                    })
                    .collect()
            }),
        }
    }

    #[tokio::test]
    async fn edits_workspace_file_with_one_exact_line_ranged_replacement() {
        let workspace_dir =
            make_temp_dir("edits_workspace_file_with_one_exact_line_ranged_replacement");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", &[(2, 2, "beta", "gamma")]),
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
    async fn applies_multiple_edits_against_original_line_numbers() {
        let workspace_dir = make_temp_dir("applies_multiple_edits_against_original_line_numbers");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args(
                    "notes.txt",
                    &[(1, 1, "alpha", "one\ntwo"), (3, 3, "gamma", "three")],
                ),
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
            "one\ntwo\nbeta\nthree\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn edits_blank_line_ranges() {
        let workspace_dir = make_temp_dir("edits_blank_line_ranges");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\n\nomega\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", &[(2, 2, "", "beta")]),
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
            "alpha\nbeta\nomega\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn initializes_existing_empty_file() {
        let workspace_dir = make_temp_dir("initializes_existing_empty_file");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", &[(1, 1, "", "alpha")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Success { data } => assert_eq!(data, json!({ "success": true })),
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }
        assert_eq!(fs::read_to_string(path).expect("read file"), "alpha");

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn fails_when_old_text_does_not_match_selected_lines() {
        let workspace_dir = make_temp_dir("fails_when_old_text_does_not_match_selected_lines");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", &[(2, 2, "missing", "gamma")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("lines 2-2 do not match old_text"))
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }
        assert_eq!(
            fs::read_to_string(path).expect("read file"),
            "alpha\nbeta\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn fails_when_line_range_is_out_of_bounds() {
        let workspace_dir = make_temp_dir("fails_when_line_range_is_out_of_bounds");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", &[(3, 3, "gamma", "delta")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => assert!(message.contains("out of bounds")),
            ToolOutput::Success { .. } => panic!("expected error"),
        }
        assert_eq!(
            fs::read_to_string(path).expect("read file"),
            "alpha\nbeta\n"
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn fails_when_line_range_is_reversed() {
        let workspace_dir = make_temp_dir("fails_when_line_range_is_reversed");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args("notes.txt", &[(2, 1, "beta", "gamma")]),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => assert!(message.contains("invalid line range")),
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn fails_when_edit_ranges_overlap() {
        let workspace_dir = make_temp_dir("fails_when_edit_ranges_overlap");
        let path = workspace_dir.join("notes.txt");
        fs::write(&path, "alpha\nbeta\ngamma\n").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                edit_args(
                    "notes.txt",
                    &[
                        (1, 2, "alpha\nbeta", "one\ntwo"),
                        (2, 3, "beta\ngamma", "two\nthree"),
                    ],
                ),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => assert!(message.contains("ranges overlap")),
            ToolOutput::Success { .. } => panic!("expected error"),
        }
        assert_eq!(
            fs::read_to_string(path).expect("read file"),
            "alpha\nbeta\ngamma\n"
        );

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
                edit_args(
                    "notes.txt",
                    &[(1, 1, "alpha", "one"), (2, 2, "missing", "two")],
                ),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("lines 2-2 do not match old_text"))
            }
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
                create_args("created.txt", "hello\n"),
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
    async fn create_mode_rejects_edits() {
        let workspace_dir = make_temp_dir("create_mode_rejects_edits");
        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                invalid_args(
                    "created.txt",
                    true,
                    Some("hello\n"),
                    Some(&[(1, 1, "", "hello\n")]),
                ),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("create=true requires edits to be omitted"))
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn edit_mode_rejects_contents() {
        let workspace_dir = make_temp_dir("edit_mode_rejects_contents");
        fs::write(workspace_dir.join("notes.txt"), "alpha").expect("write file");

        let tool = EditFileTool;
        let config = tool_config(&workspace_dir);
        let output = tool
            .call(
                invalid_args(
                    "notes.txt",
                    false,
                    Some("hello\n"),
                    Some(&[(1, 1, "alpha", "beta")]),
                ),
                &ToolContext {
                    global_config: &config,
                },
            )
            .await;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("create=false requires contents to be omitted"))
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

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
                create_args("created.txt", "hello\n"),
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
                create_args("missing/created.txt", "hello\n"),
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
            &edit_args("notes.txt", &[(1, 1, "a", "b")]),
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
                &[(1, 1, "a", "b")],
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
        let description = tool.describe(&edit_args("src/lib.rs", &[(1, 1, "old", "new")]));

        assert_eq!(description, "Edit file src/lib.rs with 1 replacement(s)");
    }

    #[test]
    fn native_toon_edit_arguments_validate_successfully() {
        let args: serde_json::Value = decode_default(
            "path: notes.txt\ncreate: false\nedits[1]{start_line,end_line,old_text,new_text}:\n  2,2,beta,gamma",
        )
        .expect("decode native toon edit args");
        let mut manager = tool_core::ToolManager::new();
        manager.register_tool(EditFileTool);
        manager
            .prepare_tool(&types::ToolId::new("edit_file"), args)
            .expect("validate native edit args");
    }

    #[test]
    fn native_toon_edit_arguments_require_line_numbers() {
        let args: serde_json::Value = decode_default(
            "path: notes.txt\ncreate: false\nedits[1]{old_text,new_text}:\n  beta,gamma",
        )
        .expect("decode native toon edit args");
        let mut manager = tool_core::ToolManager::new();
        manager.register_tool(EditFileTool);
        let error = match manager.prepare_tool(&types::ToolId::new("edit_file"), args) {
            Ok(_) => panic!("missing line numbers should fail validation"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("missing field `start_line`"));
    }
}
