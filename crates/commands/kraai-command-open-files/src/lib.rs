#![forbid(unsafe_code)]

use std::path::Path;

use kraai_command_core::declare_kraai_command;
use kraai_types::{ContextStateDelta, SandboxCapability};
use kraai_workspace_fs::validate_text_file;
use nu_engine::CallExt;
use nu_protocol::shell_error::generic::GenericError;
use nu_protocol::{
    Category, IntoPipelineData, ShellError, Signature, Span, SyntaxShape, Type, Value, record,
};

const COMMAND_ID: &str = "kraai-open-files";

declare_kraai_command! {
    /// Pins files for fresh context injection on subsequent model turns.
    pub struct OpenFilesCommand;
    id: "kraai-open-files";
    name: "kraai-open-files";
    description: "Pin one or more text files for fresh context injection on future turns.";
    signature_help: "kraai-open-files <path>... -> record<success: bool, paths: list<string>>";
    capabilities: [SandboxCapability::WorkspaceRead];
    examples: [
        {
            description: "Pin a source file for future turns",
            timeout: "10sec",
            script: "kraai-open-files src/main.rs",
        },
        {
            description: "Pin several files without returning their contents",
            timeout: "10sec",
            script: "kraai-open-files Cargo.toml src/lib.rs",
        },
    ];
    signature: Signature::build("kraai-open-files")
        .rest(
            "paths",
            SyntaxShape::String,
            "Text file paths to keep in the context of future turns.",
        )
        .input_output_types(vec![(Type::Nothing, Type::Record(Default::default()))])
        .category(Category::Experimental);
    run: |context, engine_state, stack, call, _input| {
        let paths: Vec<String> = call.rest(engine_state, stack, 0)?;
        if paths.is_empty() {
            return Err(command_error(
                "Missing file path",
                "kraai-open-files requires at least one path",
                call.head,
            ));
        }

        let cwd = engine_state.cwd(Some(stack))?;
        let mut opened = Vec::with_capacity(paths.len());
        for path in paths {
            let normalized = validate_text_file(cwd.as_ref(), Path::new(&path)).map_err(|error| {
                command_error("Unable to open file", error.to_string(), call.head)
            })?;
            let normalized_string = normalized.to_string_lossy().into_owned();
            context
                .state_effects()
                .apply(
                    COMMAND_ID,
                    vec![ContextStateDelta {
                        namespace: String::from("opened_files"),
                        operation: String::from("open"),
                        payload: serde_json::json!({ "path": normalized_string }),
                    }],
                )
                .map_err(|error| {
                    command_error(
                        "Unable to persist opened file",
                        error.to_string(),
                        call.head,
                    )
                })?;
            opened.push(Value::string(normalized_string, call.head));
        }

        Ok(Value::record(
            record! {
                "success" => Value::bool(true, call.head),
                "paths" => Value::list(opened, call.head),
            },
            call.head,
        )
        .into_pipeline_data())
    }
}

fn command_error(title: impl Into<String>, message: impl Into<String>, span: Span) -> ShellError {
    let title: String = title.into();
    let message: String = message.into();
    ShellError::Generic(GenericError::new(title, message, span))
}
