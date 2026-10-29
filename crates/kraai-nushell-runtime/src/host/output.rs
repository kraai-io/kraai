use nu_engine::command_prelude::*;
use std::io::Write;
use std::sync::Arc;

use nu_protocol::{ByteStream, ByteStreamSource};

use super::HostError;
use super::sink::OutputSink;
use crate::rendering::OutputValue;

pub(super) fn register(
    working_set: &mut StateWorkingSet<'_>,
    sink: &Arc<OutputSink>,
) -> Result<(), HostError> {
    let id = working_set
        .find_decl(b"print")
        .ok_or_else(|| HostError::Initialization("Nushell print command is missing".into()))?;
    let original = working_set.get_decl(id).clone_box();
    working_set.add_decl(Box::new(ModelPrint {
        original,
        sink: sink.clone(),
    }));
    Ok(())
}

#[derive(Clone)]
struct ModelPrint {
    original: Box<dyn Command>,
    sink: Arc<OutputSink>,
}

impl Command for ModelPrint {
    fn name(&self) -> &str {
        "print"
    }

    fn signature(&self) -> Signature {
        self.original.signature()
    }

    fn description(&self) -> &str {
        self.original.description()
    }

    fn run(
        &self,
        engine: &EngineState,
        stack: &mut Stack,
        call: &Call,
        mut input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        if call.has_flag(engine, stack, "raw")? {
            return self.original.run(engine, stack, call, input);
        }
        let args: Vec<Value> = call.rest(engine, stack, 0)?;
        let no_newline = call.has_flag(engine, stack, "no-newline")?;
        let stderr = call.has_flag(engine, stack, "stderr")?;
        if args.is_empty() {
            if let PipelineData::ByteStream(stream, _) = &mut input
                && let ByteStreamSource::Child(child) = stream.source_mut()
            {
                child.ignore_error(true);
            }
            render(&self.sink, engine, input, no_newline, stderr).map_err(|error| *error)?;
        } else {
            for arg in args {
                render(
                    &self.sink,
                    engine,
                    arg.into_pipeline_data(),
                    no_newline,
                    stderr,
                )
                .map_err(|error| *error)?;
            }
        }
        Ok(PipelineData::empty())
    }
}

pub(super) fn emit(
    sink: &OutputSink,
    engine: &EngineState,
    stack: &mut Stack,
    input: PipelineData,
) -> Result<(), Box<ShellError>> {
    let stream_span = match &input {
        PipelineData::ByteStream(stream, _) => Some(stream.span()),
        _ => None,
    };
    let result = render(sink, engine, input, false, false);
    if let Some(span) = stream_span {
        match &result {
            Ok(()) => stack.set_last_exit_code(0, span),
            Err(error) => stack.set_last_error(error),
        }
    }
    result
}

fn render(
    sink: &OutputSink,
    engine: &EngineState,
    input: PipelineData,
    no_newline: bool,
    stderr: bool,
) -> Result<(), Box<ShellError>> {
    match input {
        PipelineData::Empty | PipelineData::Value(Value::Nothing { .. }, _) => Ok(()),
        PipelineData::ByteStream(stream, _) => write_bytes(engine, stream, no_newline, stderr),
        PipelineData::Value(
            Value::Binary {
                val, internal_span, ..
            },
            _,
        ) => write_bytes(
            engine,
            ByteStream::read_binary(val, internal_span, engine.signals().clone()),
            no_newline,
            stderr,
        ),
        PipelineData::Value(value @ Value::String { .. }, _) => value
            .into_pipeline_data()
            .print_raw(engine, no_newline, stderr)
            .map_err(Box::new),
        PipelineData::Value(Value::List { vals, .. }, _) if vals.is_empty() => {
            sink.value(OutputValue::List(Vec::new()), stderr, !no_newline)
        }
        PipelineData::Value(value @ (Value::List { .. } | Value::Range { .. }), _) => {
            let span = value.span();
            render_list(
                sink,
                engine,
                value.into_pipeline_data(),
                span,
                no_newline,
                stderr,
            )
        }
        PipelineData::ListStream(stream, _) => {
            let span = stream.span();
            render_list(sink, engine, stream, span, no_newline, stderr)
        }
        PipelineData::Value(value, _) => {
            sink.value(OutputValue::from_nu(value, engine)?, stderr, !no_newline)
        }
    }
}

fn render_list(
    sink: &OutputSink,
    engine: &EngineState,
    values: impl IntoIterator<Item = Value>,
    span: Span,
    no_newline: bool,
    stderr: bool,
) -> Result<(), Box<ShellError>> {
    let list = sink.open_list();
    let mut sent = false;
    let result = values.into_iter().try_for_each(|value| {
        engine.signals().check(&span)?;
        sink.item(list, OutputValue::from_nu(value, engine)?)?;
        sent = true;
        Ok::<_, Box<ShellError>>(())
    });
    let closed = if sent {
        sink.close_list(list, stderr, !no_newline)
    } else {
        Ok(())
    };
    result.and(closed)
}

fn write_bytes(
    engine: &EngineState,
    stream: ByteStream,
    no_newline: bool,
    stderr: bool,
) -> Result<(), Box<ShellError>> {
    let span = stream.span();
    let last = if stderr {
        copy_bytes(stream, std::io::stderr())
    } else {
        copy_bytes(stream, std::io::stdout())
    }?;
    if no_newline || last.is_none_or(|byte| byte == b'\n') {
        return Ok(());
    }
    Value::string("", span)
        .into_pipeline_data()
        .print_raw(engine, false, stderr)
        .map_err(Box::new)
}

fn copy_bytes(stream: ByteStream, dest: impl Write) -> Result<Option<u8>, Box<ShellError>> {
    let mut dest = LastByte { dest, last: None };
    stream.write_to(&mut dest)?;
    Ok(dest.last)
}

struct LastByte<W> {
    dest: W,
    last: Option<u8>,
}

impl<W: Write> Write for LastByte<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.dest.write(buf)?;
        if let Some(&byte) = buf.get(..written).and_then(<[u8]>::last) {
            self.last = Some(byte);
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.dest.flush()
    }
}

pub(super) fn configure(engine: &mut EngineState, stack: &mut Stack) {
    let mut config = (*stack.get_config(engine)).clone();
    config.error_style = nu_protocol::ErrorStyle::Plain;
    config.use_ansi_coloring = nu_protocol::UseAnsiColoring::False;
    config.display_errors.exit_code = true;
    config.color_config.extend(
        nu_protocol::default_color_config()
            .into_keys()
            .map(|key| (key, Value::record(Record::new(), Span::unknown()))),
    );
    stack.add_env_var("config".into(), config.clone().into_value(Span::unknown()));
    let config = std::sync::Arc::new(config);
    stack.config = Some(config.clone());
    engine.set_config(config);
}
