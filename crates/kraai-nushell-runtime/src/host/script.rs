use nu_protocol::debugger::WithoutDebug;
use nu_protocol::engine::Redirection;
use nu_protocol::engine::{EngineState, Stack, StateWorkingSet};
use nu_protocol::{OutDest, PipelineData, ShellError};

use super::report;
use super::sink::OutputSink;

pub(super) const SCRIPT_NAME: &str = "kraai-script.nu";

pub(super) fn evaluate(
    sink: &OutputSink,
    engine: &mut EngineState,
    stack: &mut Stack,
    source: &[u8],
) -> i32 {
    match evaluate_source(sink, engine, stack, source) {
        Ok(()) => 0,
        Err(error) => {
            if let ShellError::Exit { code, .. } = *error {
                return code;
            }
            report::shell_error(sink, stack, engine, &error);
            let code = error.exit_code().unwrap_or(1);
            stack.set_last_error(&error);
            code
        }
    }
}

fn evaluate_source(
    sink: &OutputSink,
    engine: &mut EngineState,
    stack: &mut Stack,
    source: &[u8],
) -> Result<(), Box<ShellError>> {
    let (blocks, delta) = {
        let mut working_set = StateWorkingSet::new(engine);
        let block = nu_parser::parse(&mut working_set, Some(SCRIPT_NAME), source, false);
        if let Some(warning) = working_set.parse_warnings.first() {
            report::parse_warning(sink, &working_set, warning);
        }
        if let Some(error) = working_set.parse_errors.first() {
            report::parse_error(sink, &working_set, error);
            return Err(Box::new(ShellError::Exit {
                code: 1,
                abort: false,
            }));
        }
        if let Some(error) = working_set.compile_errors.first() {
            report::compile_error(sink, &working_set, error);
            return Err(Box::new(ShellError::Exit {
                code: 1,
                abort: false,
            }));
        }
        let mut template = (*block).clone();
        let pipelines = std::mem::take(&mut template.pipelines);
        template.ir_block = None;
        let emit = super::statement_output::emit_decl(&working_set).ok_or_else(|| {
            Box::new(ShellError::NushellFailed {
                msg: "statement output command is missing".into(),
            })
        })?;
        let mut blocks = Vec::with_capacity(pipelines.len());
        for pipeline in pipelines {
            let assignment = super::statement_output::is_assignment(&working_set, &pipeline);
            let mut statement = template.clone();
            statement.pipelines.push(pipeline);
            match nu_engine::compile(&working_set, &statement) {
                Ok(mut ir) => {
                    if !assignment {
                        super::statement_output::emit_drained_statements(&mut ir, emit);
                    }
                    statement.ir_block = Some(ir);
                }
                Err(error) => {
                    report::compile_error(sink, &working_set, &error);
                    return Err(Box::new(ShellError::Exit {
                        code: 1,
                        abort: false,
                    }));
                }
            }
            blocks.push(statement);
        }
        (blocks, working_set.render())
    };
    engine.merge_delta(delta)?;
    let stderr = stack.pipe_stderr().cloned().map(Redirection::Pipe);
    let stack = &mut stack.push_redirection(Some(Redirection::Pipe(OutDest::Pipe)), stderr);
    for block in blocks {
        let pipeline =
            nu_engine::eval_block::<WithoutDebug>(engine, stack, &block, PipelineData::empty())?;
        super::apply_variable_deletions(engine, stack);
        super::output::emit(sink, engine, stack, pipeline.body)?;
        if nu_experimental::PIPE_FAIL.get() {
            nu_protocol::process::check_exit_status_future(pipeline.exit)?;
        }
        if pipeline.early_return {
            break;
        }
    }
    Ok(())
}
