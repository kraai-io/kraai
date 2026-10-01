use nu_engine::command_prelude::*;
use nu_protocol::ByteStreamSource;

use super::HostError;

pub(super) fn register(working_set: &mut StateWorkingSet<'_>) -> Result<(), HostError> {
    let id = working_set
        .find_decl(b"print")
        .ok_or_else(|| HostError::Initialization("Nushell print command is missing".into()))?;
    let original = working_set.get_decl(id).clone_box();
    working_set.add_decl(Box::new(ModelPrint { original }));
    Ok(())
}

#[derive(Clone)]
struct ModelPrint {
    original: Box<dyn Command>,
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
            render(engine, input, no_newline, stderr).map_err(|error| *error)?;
        } else {
            for arg in args {
                render(engine, arg.into_pipeline_data(), no_newline, stderr)
                    .map_err(|error| *error)?;
            }
        }
        Ok(PipelineData::empty())
    }
}

pub(super) fn render(
    engine: &EngineState,
    input: PipelineData,
    no_newline: bool,
    stderr: bool,
) -> Result<(), Box<ShellError>> {
    match input {
        PipelineData::Empty | PipelineData::Value(Value::Nothing { .. }, _) => Ok(()),
        PipelineData::ByteStream(stream, _) => stream.print(stderr).map_err(Box::new),
        PipelineData::Value(value @ (Value::String { .. } | Value::Binary { .. }), _) => value
            .into_pipeline_data()
            .print_raw(engine, no_newline, stderr)
            .map_err(Box::new),
        PipelineData::Value(value @ (Value::List { .. } | Value::Range { .. }), _) => {
            let span = value.span();
            render_list(engine, value.into_pipeline_data(), span, no_newline, stderr)
        }
        PipelineData::ListStream(stream, _) => {
            let span = stream.span();
            for value in stream {
                engine.signals().check(&span)?;
                json(engine, value)?.print_raw(engine, no_newline, stderr)?;
            }
            Ok(())
        }
        PipelineData::Value(value, _) => json(engine, value)?
            .print_raw(engine, no_newline, stderr)
            .map_err(Box::new),
    }
}

fn json(engine: &EngineState, value: Value) -> Result<PipelineData, Box<ShellError>> {
    let span = value.span();
    let from_type = value.get_type().to_string();
    if let Value::Error { error, .. } = value {
        return Err(error);
    }
    let value = nested_json(engine, value)?;
    let json = nu_json::to_string_raw(&value).map_err(|error| {
        Box::new(ShellError::CantConvert {
            to_type: "JSON".into(),
            from_type,
            span,
            help: Some(error.to_string()),
        })
    })?;
    Ok(Value::string(json, span).into_pipeline_data())
}

fn nested_json(engine: &EngineState, value: Value) -> Result<nu_json::Value, Box<ShellError>> {
    match value {
        Value::Error { error, .. } => Ok(nu_json::Value::String(error.to_string())),
        Value::Record { val, .. } => Ok(nu_json::Value::Object(
            val.into_owned()
                .into_iter()
                .map(|(key, value)| nested_json(engine, value).map(|value| (key, value)))
                .collect::<Result<_, _>>()?,
        )),
        Value::List { vals, .. } => Ok(nu_json::Value::Array(
            vals.into_iter()
                .map(|value| nested_json(engine, value))
                .collect::<Result<_, _>>()?,
        )),
        value => nu_json::Value::from_value_serialized(value, engine).map_err(Box::new),
    }
}

fn render_list(
    engine: &EngineState,
    values: impl IntoIterator<Item = Value>,
    span: Span,
    no_newline: bool,
    stderr: bool,
) -> Result<(), Box<ShellError>> {
    let write = |text: &str, no_newline| {
        Value::string(text, span)
            .into_pipeline_data()
            .print_raw(engine, no_newline, stderr)
            .map_err(Box::new)
    };
    write("[", true)?;
    let result = (|| -> Result<(), Box<ShellError>> {
        for (index, value) in values.into_iter().enumerate() {
            engine.signals().check(&span)?;
            let value = json(engine, value)?;
            if index != 0 {
                write(",", true)?;
            }
            value.print_raw(engine, true, stderr)?;
        }
        Ok(())
    })();
    let closing = write("]", no_newline);
    result.and(closing)
}
