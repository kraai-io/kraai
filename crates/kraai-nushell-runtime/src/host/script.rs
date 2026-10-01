use nu_protocol::PipelineData;
use nu_protocol::ShellError;
use nu_protocol::debugger::WithoutDebug;
use nu_protocol::engine::{EngineState, Stack, StateWorkingSet};
use nu_protocol::report_error::{
    report_compile_error, report_parse_error, report_parse_warning, report_shell_error,
};

pub(super) fn evaluate(engine: &mut EngineState, stack: &mut Stack, source: &[u8]) -> i32 {
    match evaluate_source(engine, stack, source) {
        Ok(()) => 0,
        Err(error) => {
            if let ShellError::Exit { code, .. } = *error {
                return code;
            }
            report_shell_error(Some(stack), engine, &error);
            let code = error.exit_code().unwrap_or(1);
            stack.set_last_error(&error);
            code
        }
    }
}

fn evaluate_source(
    engine: &mut EngineState,
    stack: &mut Stack,
    source: &[u8],
) -> Result<(), Box<ShellError>> {
    let (blocks, delta) = {
        let mut working_set = StateWorkingSet::new(engine);
        let block = nu_parser::parse(&mut working_set, Some("kraai-script.nu"), source, false);
        if let Some(warning) = working_set.parse_warnings.first() {
            report_parse_warning(Some(stack), &working_set, warning);
        }
        if let Some(error) = working_set.parse_errors.first() {
            report_parse_error(Some(stack), &working_set, error);
            return Err(Box::new(ShellError::Exit {
                code: 1,
                abort: false,
            }));
        }
        if let Some(error) = working_set.compile_errors.first() {
            report_compile_error(Some(stack), &working_set, error);
            return Err(Box::new(ShellError::Exit {
                code: 1,
                abort: false,
            }));
        }
        let mut template = (*block).clone();
        let pipelines = std::mem::take(&mut template.pipelines);
        template.ir_block = None;
        let mut blocks = Vec::with_capacity(pipelines.len());
        for pipeline in pipelines {
            let mut statement = template.clone();
            statement.pipelines.push(pipeline);
            match nu_engine::compile(&working_set, &statement) {
                Ok(ir) => statement.ir_block = Some(ir),
                Err(error) => {
                    report_compile_error(Some(stack), &working_set, &error);
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
    for block in blocks {
        let pipeline =
            nu_engine::eval_block::<WithoutDebug>(engine, stack, &block, PipelineData::empty())?;
        super::apply_variable_deletions(engine, stack);
        super::output::render(engine, pipeline.body, false, false)?;
        if nu_experimental::PIPE_FAIL.get() {
            nu_protocol::process::check_exit_status_future(pipeline.exit)?;
        }
        if pipeline.early_return {
            break;
        }
    }
    Ok(())
}
