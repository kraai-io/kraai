use std::sync::atomic::Ordering;

use kraai_types::NushellStartup;
use nu_protocol::debugger::WithoutDebug;
use nu_protocol::engine::{EngineState, Stack, StateWorkingSet};
use nu_protocol::report_error::{SUPPRESS_REPORTING, format_cli_error};
use nu_protocol::{PipelineData, ShellError, Span};

use super::HostError;
use crate::request::HostRequest;

struct StartupReporting(bool);

impl StartupReporting {
    fn silence() -> Self {
        Self(SUPPRESS_REPORTING.swap(true, Ordering::Relaxed))
    }
}

impl Drop for StartupReporting {
    fn drop(&mut self) {
        SUPPRESS_REPORTING.store(self.0, Ordering::Relaxed);
    }
}

pub(super) fn load_startup_files(
    request: &HostRequest,
    engine_state: &mut EngineState,
    stack: &mut Stack,
) -> Result<(), HostError> {
    if request.nushell_startup != NushellStartup::Inherit || !engine_state.config_dirs.is_resolved()
    {
        return Ok(());
    }
    let _reporting = StartupReporting::silence();
    for path in [
        engine_state.config_dirs.env_file.to_path_buf(),
        engine_state.config_dirs.config_file.to_path_buf(),
    ] {
        let contents = match std::fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(HostError::Initialization(format!(
                    "unable to read startup file '{}': {error}",
                    path.display()
                )));
            }
        };
        let previous_file = engine_state.file.replace(path.clone());
        let result = evaluate_startup(engine_state, stack, &contents, &path.to_string_lossy());
        engine_state.file = previous_file;
        result.map_err(|diagnostic| {
            HostError::Initialization(format!(
                "startup file '{}' failed; script was not executed\n{diagnostic}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn evaluate_startup(
    engine_state: &mut EngineState,
    stack: &mut Stack,
    contents: &[u8],
    filename: &str,
) -> Result<(), String> {
    let (block, delta) = {
        let mut working_set = StateWorkingSet::new(engine_state);
        let block = nu_parser::parse(&mut working_set, Some(filename), contents, false);
        if let Some(error) = working_set.parse_errors.first() {
            return Err(format_cli_error(Some(stack), &working_set, error, None));
        }
        if let Some(error) = working_set.compile_errors.first() {
            return Err(format_cli_error(Some(stack), &working_set, error, None));
        }
        (block, working_set.render())
    };
    let result = (|| -> Result<(), Box<ShellError>> {
        engine_state.merge_delta(delta)?;
        let pipeline = nu_engine::eval_block::<WithoutDebug>(
            engine_state,
            stack,
            &block,
            PipelineData::empty(),
        )?;
        super::apply_variable_deletions(engine_state, stack);
        let no_newline = matches!(&pipeline.body, PipelineData::ByteStream(..));
        pipeline
            .body
            .print_table(engine_state, stack, no_newline, false)?;
        nu_protocol::process::check_exit_status_future(pipeline.exit)?;
        engine_state.merge_env(stack)?;
        stack.set_last_exit_code(0, Span::unknown());
        Ok(())
    })();
    result.map_err(|error| {
        format_cli_error(
            Some(stack),
            &StateWorkingSet::new(engine_state),
            error.as_ref(),
            None,
        )
    })
}
