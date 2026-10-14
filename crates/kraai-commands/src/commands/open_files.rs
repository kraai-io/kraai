use crate::CommandMetadata;

pub const METADATA: CommandMetadata = command_metadata! {
    id: "kraai-open-files";
    description: "Read text files into context as line-numbered snapshots. Contents stay available and refresh automatically when changed; no reread needed.";
    signature_help: "kraai-open-files <path>... -> record<success: bool, paths: list<string>>";
    services: [StateEffects];
    examples: [];
};

#[cfg(feature = "nushell")]
pub use command::OpenFilesCommand;

#[cfg(feature = "nushell")]
mod command {
    use std::path::Path;

    use kraai_execution_types::OpenedFilesOperation;
    use kraai_workspace_fs::validate_text_file;
    use nu_engine::CallExt;
    use nu_protocol::{Category, IntoPipelineData, Signature, SyntaxShape, Type, Value, record};

    use crate::{command_error, declare_kraai_command};

    declare_kraai_command! {
        /// Pins files for fresh context injection on subsequent model turns.
        pub struct OpenFilesCommand;
        metadata: super::METADATA;
        signature: Signature::build(Self::METADATA.name)
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
}
