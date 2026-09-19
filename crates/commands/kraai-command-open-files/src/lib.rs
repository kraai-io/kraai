#![forbid(unsafe_code)]

use std::path::Path;

use kraai_command_core::{command_error, declare_kraai_command};
use kraai_types::OpenedFilesOperation;
use kraai_workspace_fs::validate_text_file;
use nu_engine::CallExt;
use nu_protocol::{Category, IntoPipelineData, Signature, SyntaxShape, Type, Value, record};

declare_kraai_command! {
    /// Pins files for fresh context injection on subsequent model turns.
    pub struct OpenFilesCommand;
    metadata: kraai_command_catalog::OPEN_FILES;
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
                    Self::METADATA.id,
                    vec![OpenedFilesOperation::Open.into_delta(normalized_string.clone())],
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
