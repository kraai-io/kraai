use nu_protocol::engine::EngineState;
use nu_protocol::{ShellError, Value};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum OutputValue {
    Nothing,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Literal(String),
    Binary(Vec<u8>),
    List(Vec<OutputValue>),
    Record(Vec<(String, OutputValue)>),
}

impl OutputValue {
    pub(crate) fn from_nu(value: Value, engine: &EngineState) -> Result<Self, Box<ShellError>> {
        if let Value::Error { error, .. } = value {
            return Err(error);
        }
        Ok(Self::nested(value, engine))
    }

    pub(crate) fn size_hint(&self) -> usize {
        const NODE: usize = 16;
        NODE + match self {
            Self::Text(text) | Self::Literal(text) => text.len(),
            Self::Binary(bytes) => bytes.len() * 2,
            Self::List(items) => items.iter().map(Self::size_hint).sum(),
            Self::Record(fields) => fields
                .iter()
                .map(|(key, value)| NODE + key.len() + value.size_hint())
                .sum(),
            Self::Nothing | Self::Bool(_) | Self::Int(_) | Self::Float(_) => 8,
        }
    }

    fn nested(value: Value, engine: &EngineState) -> Self {
        match value {
            Value::Nothing { .. } => Self::Nothing,
            Value::Bool { val, .. } => Self::Bool(val),
            Value::Int { val, .. } => Self::Int(val),
            Value::Float { val, .. } => Self::Float(val),
            Value::String { val, .. } => Self::Text(val),
            Value::Glob { val, .. } => Self::Text(val),
            Value::Binary { val, .. } => Self::Binary(val.into_owned()),
            Value::Date { val, .. } => Self::Literal(val.to_rfc3339()),
            Value::Error { error, .. } => Self::Text(error.to_string()),
            Value::List { vals, .. } => Self::List(
                vals.into_iter()
                    .map(|value| Self::nested(value, engine))
                    .collect(),
            ),
            Value::Record { val, .. } => Self::Record(
                val.into_owned()
                    .into_iter()
                    .map(|(key, value)| (key, Self::nested(value, engine)))
                    .collect(),
            ),
            Value::Custom {
                val, internal_span, ..
            } => match val.to_base_value(internal_span) {
                Ok(value) => Self::nested(value, engine),
                Err(error) => Self::Text(error.to_string()),
            },
            value => Self::Literal(value.to_expanded_string(", ", engine.get_config())),
        }
    }
}
