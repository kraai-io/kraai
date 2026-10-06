use super::boxed_error as command_error;
use nu_protocol::{Record, ShellError, Span, Value};
use serde_json::Value as Json;

pub(super) fn to_json(value: Value) -> Result<Json, Box<ShellError>> {
    let span = value.span();
    match value {
        Value::Nothing { .. } => Ok(Json::Null),
        Value::Bool { val, .. } => Ok(Json::Bool(val)),
        Value::Int { val, .. } => Ok(val.into()),
        Value::Float { val, .. } => serde_json::Number::from_f64(val)
            .map(Json::Number)
            .ok_or_else(|| {
                command_error("Invalid MCP arguments", "JSON numbers must be finite", span)
            }),
        Value::String { val, .. } => Ok(Json::String(val)),
        Value::List { vals, .. } => vals
            .into_iter()
            .map(to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Json::Array),
        Value::Record { val, .. } => val
            .into_owned()
            .into_iter()
            .map(|(key, value)| to_json(value).map(|value| (key, value)))
            .collect::<Result<_, _>>()
            .map(Json::Object),
        Value::Error { error, .. } => Err(error),
        _ => Err(command_error(
            "Invalid MCP arguments",
            "Use JSON-compatible records, lists, strings, numbers, booleans, or null",
            span,
        )),
    }
}

pub(super) fn from_json(value: Json, span: Span) -> Result<Value, Box<ShellError>> {
    Ok(match value {
        Json::Null => Value::nothing(span),
        Json::Bool(value) => Value::bool(value, span),
        Json::String(value) => Value::string(value, span),
        Json::Number(value) => {
            if let Some(value) = value.as_i64() {
                Value::int(value, span)
            } else if value.is_f64() {
                Value::float(
                    value.as_f64().ok_or_else(|| {
                        command_error("Invalid MCP result", "Unrepresentable number", span)
                    })?,
                    span,
                )
            } else {
                return Err(command_error(
                    "Invalid MCP result",
                    "Integer exceeds Nushell's signed 64-bit range",
                    span,
                ));
            }
        }
        Json::Array(values) => Value::list(
            values
                .into_iter()
                .map(|value| from_json(value, span))
                .collect::<Result<_, _>>()?,
            span,
        ),
        Json::Object(values) => {
            let mut record = Record::new();
            for (name, value) in values {
                record.push(name, from_json(value, span)?);
            }
            Value::record(record, span)
        }
    })
}
