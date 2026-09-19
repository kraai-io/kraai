#![forbid(unsafe_code)]

use std::path::Path;

use kraai_command_core::{command_error, declare_kraai_command};
use kraai_types::OpenedFilesOperation;
use kraai_workspace_fs::normalize_allow_missing;
use nu_engine::CallExt;
use nu_protocol::{Category, IntoPipelineData, Signature, SyntaxShape, Type, Value, record};

declare_kraai_command! {
    /// Removes files from fresh context injection on subsequent model turns.
    pub struct CloseFilesCommand;
    metadata: kraai_command_catalog::CLOSE_FILES;
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
                    Self::METADATA.id,
                    vec![OpenedFilesOperation::Close.into_delta(normalized_string.clone())],
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
