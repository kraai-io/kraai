#![forbid(unsafe_code)]

use std::path::Path;

use kraai_command_core::declare_kraai_command;
use kraai_types::ContextStateDelta;
use kraai_workspace_fs::normalize_allow_missing;
use nu_engine::CallExt;
use nu_protocol::shell_error::generic::GenericError;
use nu_protocol::{
    Category, IntoPipelineData, ShellError, Signature, Span, SyntaxShape, Type, Value, record,
};

const COMMAND_ID: &str = "kraai-close-files";

declare_kraai_command! {
    /// Removes files from fresh context injection on subsequent model turns.
    pub struct CloseFilesCommand;
    id: "kraai-close-files";
    name: "kraai-close-files";
    description: "Stop pinning one or more files in the context of future turns.";
    signature_help: "kraai-close-files <path>... -> record<success: bool, paths: list<string>>";
    capabilities: [];
    examples: [
        {
            description: "Remove a file that is no longer needed from future context",
            timeout: "10sec",
            script: "kraai-close-files src/main.rs",
        },
    ];
    signature: Signature::build("kraai-close-files")
        .rest(
            "paths",
            SyntaxShape::String,
            "File paths to remove from the context of future turns.",
        )
        .input_output_types(vec![(Type::Nothing, Type::Record(Default::default()))])
        .category(Category::Experimental);
    run: |context, engine_state, stack, call, _input| {
        let paths: Vec<String> = call.rest(engine_state, stack, 0)?;
        if paths.is_empty() {
            return Err(command_error(
                "Missing file path",
                "kraai-close-files requires at least one path",
                call.head,
            ));
        }

        let cwd = engine_state.cwd(Some(stack))?;
        let mut closed = Vec::with_capacity(paths.len());
        for path in paths {
            let normalized = normalize_allow_missing(cwd.as_ref(), Path::new(&path));
            let normalized_string = normalized.to_string_lossy().into_owned();
            context
                .state_effects()
                .apply(
                    COMMAND_ID,
                    vec![ContextStateDelta {
                        namespace: String::from("opened_files"),
                        operation: String::from("close"),
                        payload: serde_json::json!({ "path": normalized_string }),
                    }],
                )
                .map_err(|error| {
                    command_error(
                        "Unable to persist closed file",
                        error.to_string(),
                        call.head,
                    )
                })?;
            closed.push(Value::string(normalized_string, call.head));
        }

        Ok(Value::record(
            record! {
                "success" => Value::bool(true, call.head),
                "paths" => Value::list(closed, call.head),
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

#[cfg(test)]
mod tests {
    use kraai_workspace_fs::normalize_allow_missing;
    use std::path::Path;

    #[test]
    fn normalizes_missing_paths_without_requiring_the_file_to_exist() {
        assert_eq!(
            normalize_allow_missing(Path::new("/workspace/src"), Path::new("../missing.rs")),
            Path::new("/workspace/missing.rs")
        );
    }
}
