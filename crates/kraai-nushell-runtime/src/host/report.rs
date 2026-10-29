use miette::SourceCode;
use nu_protocol::engine::{EngineState, Stack, StateWorkingSet};
use nu_protocol::{CompileError, ParseError, ParseWarning, ShellError};

use super::script::SCRIPT_NAME;
use super::sink::OutputSink;
use crate::rendering::{Diagnostic, Origin};

pub(super) fn shell_error(
    sink: &OutputSink,
    stack: &Stack,
    engine: &EngineState,
    error: &ShellError,
) {
    if stack.get_config(engine).display_errors.should_show(error) {
        report(
            sink,
            &StateWorkingSet::new(engine),
            Some(stack),
            error,
            Some("nu::shell::error"),
        );
    }
}

pub(super) fn parse_error(
    sink: &OutputSink,
    working_set: &StateWorkingSet<'_>,
    error: &ParseError,
) {
    report(sink, working_set, None, error, Some("nu::parser::error"));
}

pub(super) fn parse_warning(
    sink: &OutputSink,
    working_set: &StateWorkingSet<'_>,
    warning: &ParseWarning,
) {
    report(sink, working_set, None, warning, None);
}

pub(super) fn compile_error(
    sink: &OutputSink,
    working_set: &StateWorkingSet<'_>,
    error: &CompileError,
) {
    report(sink, working_set, None, error, Some("nu::compile::error"));
}

fn report(
    sink: &OutputSink,
    working_set: &StateWorkingSet<'_>,
    stack: Option<&Stack>,
    diagnostic: &dyn miette::Diagnostic,
    default_code: Option<&str>,
) {
    let source: &dyn SourceCode = &working_set;
    let cwd = working_set.permanent().cwd(stack).ok();
    sink.diagnostic(Diagnostic::new(
        diagnostic,
        source,
        default_code,
        &Origin {
            script: SCRIPT_NAME,
            cwd: cwd.as_ref().map(|cwd| cwd.as_std_path()),
        },
    ));
}
