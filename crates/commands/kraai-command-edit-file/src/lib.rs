#![forbid(unsafe_code)]

use std::path::Path;

use kraai_command_core::declare_kraai_command;
use kraai_workspace_fs::{ExactTextEdit, create_text_file, edit_text_file};
use nu_engine::CallExt;
use nu_protocol::shell_error::generic::GenericError;
use nu_protocol::{
    Category, IntoPipelineData, ShellError, Signature, Span, SyntaxShape, Type, Value, record,
};

declare_kraai_command! {
    /// Applies deterministic, exact text edits or creates one text file.
    pub struct EditFileCommand;
    id: "kraai-edit-file";
    name: "kraai-edit-file";
    description: "Create a text file or atomically apply exact line-ranged replacements.";
    signature_help: "kraai-edit-file <path> <edits?> [--create --contents <text>] -> record<success: bool, path: string, operation: string>";
    examples: [
        {
            description: "Replace one exact source line",
            timeout: "10sec",
            script: "kraai-edit-file src/lib.rs [{start_line: 10, end_line: 10, old_text: 'let enabled = false;', new_text: 'let enabled = true;'}]",
        },
        {
            description: "Create a new text file without replacing an existing path",
            timeout: "10sec",
            script: "kraai-edit-file src/new.rs --create --contents 'pub const READY: bool = true;\n'",
        },
    ];
    signature: Signature::build("kraai-edit-file")
        .required("path", SyntaxShape::String, "Text file path to edit or create.")
        .optional(
            "edits",
            SyntaxShape::List(Box::new(SyntaxShape::Any)),
            "Exact edit records with start_line, end_line, old_text, and new_text fields.",
        )
        .switch("create", "Create a new file instead of editing an existing file.", None)
        .named(
            "contents",
            SyntaxShape::String,
            "Complete contents for --create; invalid for normal edits.",
            None,
        )
        .input_output_types(vec![(Type::Nothing, Type::Record(Default::default()))])
        .category(Category::Experimental);
    run: |_context, engine_state, stack, call, _input| {
        let path: String = call.req(engine_state, stack, 0)?;
        let edits: Option<Value> = call.opt(engine_state, stack, 1)?;
        let create = call.has_flag(engine_state, stack, "create")?;
        let contents: Option<String> = call.get_flag(engine_state, stack, "contents")?;
        let cwd = engine_state.cwd(Some(stack))?;

        let (path, operation) = if create {
            if edits.is_some() {
                return Err(command_error(
                    "Invalid create arguments",
                    "--create requires the edits argument to be omitted",
                    call.head,
                ));
            }
            let contents = contents.ok_or_else(|| {
                command_error(
                    "Missing file contents",
                    "--create requires --contents",
                    call.head,
                )
            })?;
            let path = create_text_file(cwd.as_ref(), Path::new(&path), &contents)
                .map_err(|error| command_error("Unable to create file", error.to_string(), call.head))?;
            (path, "created")
        } else {
            if contents.is_some() {
                return Err(command_error(
                    "Invalid edit arguments",
                    "--contents may only be used with --create",
                    call.head,
                ));
            }
            let edits = edits.ok_or_else(|| {
                command_error(
                    "Missing edits",
                    "normal edits require a list of exact edit records",
                    call.head,
                )
            })?;
            let edits = parse_edits(&edits, call.head)?;
            let path = edit_text_file(cwd.as_ref(), Path::new(&path), &edits)
                .map_err(|error| command_error("Unable to edit file", error.to_string(), call.head))?;
            (path, "edited")
        };

        Ok(Value::record(
            record! {
                "success" => Value::bool(true, call.head),
                "path" => Value::string(path.to_string_lossy(), call.head),
                "operation" => Value::string(operation, call.head),
            },
            call.head,
        )
        .into_pipeline_data())
    }
}

#[expect(
    clippy::result_large_err,
    reason = "native Nushell command helpers preserve ShellError diagnostics"
)]
fn parse_edits(value: &Value, span: Span) -> Result<Vec<ExactTextEdit>, ShellError> {
    let values = value.as_list()?;
    if values.is_empty() {
        return Err(command_error(
            "Missing edits",
            "at least one exact edit record is required",
            span,
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| parse_edit(value, index, span))
        .collect()
}

#[expect(
    clippy::result_large_err,
    reason = "native Nushell command helpers preserve ShellError diagnostics"
)]
fn parse_edit(value: &Value, index: usize, span: Span) -> Result<ExactTextEdit, ShellError> {
    let record = value.as_record()?;
    let edit_number = index.saturating_add(1);
    if record.len() != 4 {
        return Err(command_error(
            "Invalid edit record",
            format!(
                "edit {edit_number} must contain exactly start_line, end_line, old_text, and new_text"
            ),
            span,
        ));
    }
    let field = |name: &str| {
        record.get(name).ok_or_else(|| {
            command_error(
                "Missing edit field",
                format!("edit {edit_number} is missing {name}"),
                span,
            )
        })
    };
    let start_line = positive_line(field("start_line")?, "start_line", edit_number, span)?;
    let end_line = positive_line(field("end_line")?, "end_line", edit_number, span)?;
    let old_text = field("old_text")?.as_str()?.to_owned();
    let new_text = field("new_text")?.as_str()?.to_owned();
    Ok(ExactTextEdit {
        start_line,
        end_line,
        old_text,
        new_text,
    })
}

#[expect(
    clippy::result_large_err,
    reason = "native Nushell command helpers preserve ShellError diagnostics"
)]
fn positive_line(
    value: &Value,
    name: &str,
    edit_number: usize,
    span: Span,
) -> Result<u32, ShellError> {
    let value = value.as_int()?;
    u32::try_from(value)
        .ok()
        .filter(|line| *line > 0)
        .ok_or_else(|| {
            command_error(
                "Invalid edit line",
                format!("edit {edit_number} field {name} must be a positive 32-bit integer"),
                span,
            )
        })
}

fn command_error(title: impl Into<String>, message: impl Into<String>, span: Span) -> ShellError {
    let title: String = title.into();
    let message: String = message.into();
    ShellError::Generic(GenericError::new(title, message, span))
}
