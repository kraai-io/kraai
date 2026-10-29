use std::io::BufRead;
use std::sync::OnceLock;

use nu_engine::command_prelude::*;
use nu_protocol::{
    ByteStreamType, CompareTypes, ListStream, PipelineMetadata, shell_error::io::IoError,
};

use super::HostError;

const ROW_COMMANDS: &[&str] = &[
    "all",
    "any",
    "append",
    "chunks",
    "drop",
    "drop nth",
    "each",
    "enumerate",
    "every",
    "filter",
    "first",
    "get",
    "group-by",
    "last",
    "length",
    "par-each",
    "prepend",
    "reduce",
    "reverse",
    "shuffle",
    "skip",
    "skip until",
    "skip while",
    "sort",
    "sort-by",
    "split list",
    "str join",
    "take",
    "take until",
    "take while",
    "uniq",
    "uniq-by",
    "where",
    "window",
    "zip",
];

pub(super) fn register(working_set: &mut StateWorkingSet<'_>) -> Result<(), HostError> {
    for name in ROW_COMMANDS {
        let id = working_set.find_decl(name.as_bytes()).ok_or_else(|| {
            HostError::Initialization(format!("Nushell {name} command is missing"))
        })?;
        let original = working_set.get_decl(id).clone_box();
        working_set.add_decl(Box::new(LineInput {
            original,
            signatures: OnceLock::new(),
        }));
    }
    Ok(())
}

#[derive(Clone)]
struct LineInput {
    original: Box<dyn Command>,
    signatures: OnceLock<Signatures>,
}

#[derive(Clone)]
struct Signatures {
    original: Signature,
    lines: Signature,
}

impl LineInput {
    fn signatures(&self) -> &Signatures {
        self.signatures.get_or_init(|| {
            let original = self.original.signature();
            let mut lines = original.clone();
            let io_types = &mut lines.input_output_types;
            if !io_types.iter().any(|(input, _)| *input == Type::Any) {
                let output = io_types
                    .iter()
                    .find(|(input, _)| matches!(input, Type::List(_) | Type::Table(_)))
                    .map_or(Type::Any, |(_, output)| output.clone());
                io_types.push((Type::Any, output));
            }
            Signatures { original, lines }
        })
    }
}

impl Command for LineInput {
    fn name(&self) -> &str {
        self.original.name()
    }

    fn signature(&self) -> Signature {
        self.signatures().lines.clone()
    }

    fn description(&self) -> &str {
        self.original.description()
    }

    fn extra_description(&self) -> &str {
        self.original.extra_description()
    }

    fn search_terms(&self) -> Vec<&str> {
        self.original.search_terms()
    }

    fn run(
        &self,
        engine: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let input = match input {
            PipelineData::ByteStream(stream, metadata)
                if stream.type_() != ByteStreamType::Binary =>
            {
                text_lines(stream, metadata).map_err(|error| *error)?
            }
            input => input,
        };
        check_input(&input, &self.signatures().original, call.head).map_err(|error| *error)?;
        self.original.run(engine, stack, call, input)
    }
}

fn text_lines(
    stream: ByteStream,
    metadata: Option<PipelineMetadata>,
) -> Result<PipelineData, Box<ShellError>> {
    let span = stream.span();
    let signals = stream.signals().clone();
    let known_text = stream.type_() == ByteStreamType::String;
    let Some(mut reader) = stream.reader() else {
        return Ok(PipelineData::list_stream(
            ListStream::new(std::iter::empty(), span, signals),
            None,
        ));
    };
    let text = known_text
        || looks_like_text(
            reader
                .fill_buf()
                .map_err(|error| Box::new(IoError::new(error, span, None).into()))?,
        );
    if !text {
        return Ok(PipelineData::byte_stream(
            ByteStream::read(reader, span, signals, ByteStreamType::Binary),
            metadata,
        ));
    }
    let lines = ByteStream::read(reader, span, signals.clone(), ByteStreamType::String)
        .lines()
        .into_iter()
        .flatten()
        .map(move |line| match line {
            Ok(line) => Value::string(line, span),
            Err(error) => Value::error(error, span),
        });
    Ok(PipelineData::list_stream(
        ListStream::new(lines, span, signals),
        None,
    ))
}

fn looks_like_text(prefix: &[u8]) -> bool {
    !prefix.contains(&0)
        && std::str::from_utf8(prefix).map_or_else(|error| error.error_len().is_none(), |_| true)
}

fn check_input(
    input: &PipelineData,
    signature: &Signature,
    head: Span,
) -> Result<(), Box<ShellError>> {
    if let PipelineData::Value(Value::Error { error, .. }, _) = input {
        return Err(error.clone());
    }
    let io_types = &signature.input_output_types;
    if io_types.is_empty()
        || io_types.iter().all(|(input, _)| *input == Type::Nothing)
        || io_types.iter().any(|(ty, _)| input.is_assignable_to(ty))
    {
        return Ok(());
    }
    if let PipelineData::Empty = input {
        return Err(Box::new(ShellError::PipelineEmpty { dst_span: head }));
    }
    Err(Box::new(ShellError::OnlySupportsThisInputType {
        exp_input_type: io_types
            .iter()
            .map(|(input, _)| input.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        wrong_type: input.get_type().to_string(),
        dst_span: head,
        src_span: input.span().unwrap_or(head),
    }))
}
