//! Compile-time Toon format encoder for schema examples.

use crate::ir::{PrimitiveType, Schema, Type};
use serde_json::{Map, Value};

/// Encode all schema examples to Toon format lines at compile time.
pub fn encode_examples_toon(schema: &Schema) -> syn::Result<Vec<Vec<String>>> {
    let mut encoded = Vec::with_capacity(schema.examples.len());

    for example in &schema.examples {
        let value: Value = serde_json::from_str(example).map_err(|error| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("Invalid JSON in schema example: {}", error),
            )
        })?;

        let obj = value.as_object().ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "Schema example must decode to a JSON object",
            )
        })?;

        encoded.push(encode_single_example(schema, obj)?);
    }

    Ok(encoded)
}

fn encode_single_example(schema: &Schema, obj: &Map<String, Value>) -> syn::Result<Vec<String>> {
    let mut lines = vec![format!("tool: {}", schema.name)];

    for field in &schema.fields {
        if let Some(value) = obj.get(&field.name) {
            lines.push(value_to_toon(&field.name, &field.ty, value)?);
        }
    }

    Ok(lines)
}

fn value_to_toon(name: &str, ty: &Type, value: &Value) -> syn::Result<String> {
    match (ty, value) {
        (Type::Primitive(PrimitiveType::String), Value::String(s)) => {
            Ok(format!("{}: {}", name, toon_string(s)))
        }
        (Type::Primitive(PrimitiveType::Integer), Value::Number(n)) => {
            Ok(format!("{}: {}", name, n))
        }
        (Type::Primitive(PrimitiveType::Float), Value::Number(n)) => Ok(format!("{}: {}", name, n)),
        (Type::Primitive(PrimitiveType::Boolean), Value::Bool(b)) => Ok(format!("{}: {}", name, b)),
        (Type::Primitive(_), Value::Null) => Ok(format!("{}: null", name)),
        (Type::Array(_), Value::Null) => Ok(format!("{}[0]:", name)),
        (Type::Array(inner), Value::Array(arr)) => {
            let range_str = format!("[{}]", arr.len());
            if arr.is_empty() {
                Ok(format!("{}{}:", name, range_str))
            } else {
                let items = arr
                    .iter()
                    .map(|item| array_item_to_toon(inner, item))
                    .collect::<syn::Result<Vec<_>>>()?;
                Ok(format!("{}{}: {}", name, range_str, items.join(",")))
            }
        }
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "Type mismatch for field '{}': expected {:?}, got JSON value '{}'",
                name, ty, value
            ),
        )),
    }
}

fn array_item_to_toon(ty: &Type, value: &Value) -> syn::Result<String> {
    match (ty, value) {
        (Type::Primitive(PrimitiveType::String), Value::String(s)) => Ok(toon_string(s)),
        (Type::Primitive(PrimitiveType::Integer), Value::Number(n)) => Ok(n.to_string()),
        (Type::Primitive(PrimitiveType::Float), Value::Number(n)) => Ok(n.to_string()),
        (Type::Primitive(PrimitiveType::Boolean), Value::Bool(b)) => Ok(b.to_string()),
        (Type::Array(inner), Value::Array(arr)) => {
            let items = arr
                .iter()
                .map(|item| array_item_to_toon(inner, item))
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(format!("[{}]", items.join(",")))
        }
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "Type mismatch in array item: expected {:?}, got '{}'",
                ty, value
            ),
        )),
    }
}

fn toon_string(s: &str) -> String {
    if needs_quoting(s) {
        format!("\"{}\"", escape_string(s))
    } else {
        s.to_string()
    }
}

fn needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }

    if s != s.trim() {
        return true;
    }

    if s == "true" || s == "false" || s == "null" {
        return true;
    }

    if looks_like_number(s) {
        return true;
    }

    for c in s.chars() {
        match c {
            ':' | '"' | '\\' | '[' | ']' | '{' | '}' | ',' | '-' | '\n' | '\r' | '\t' => {
                return true;
            }
            _ => {}
        }
    }

    false
}

fn looks_like_number(s: &str) -> bool {
    let trimmed = s.trim();

    if trimmed.is_empty() {
        return false;
    }

    let mut chars = trimmed.chars().peekable();

    if chars.peek() == Some(&'-') || chars.peek() == Some(&'+') {
        chars.next();
    }

    let mut has_digit = false;
    let mut has_dot = false;
    let mut has_exp = false;

    while let Some(c) = chars.next() {
        match c {
            '0'..='9' => has_digit = true,
            '.' => {
                if has_dot || has_exp {
                    return false;
                }
                has_dot = true;
            }
            'e' | 'E' => {
                if has_exp || !has_digit {
                    return false;
                }
                has_exp = true;
                if chars.peek() == Some(&'-') || chars.peek() == Some(&'+') {
                    chars.next();
                }
            }
            _ => return false,
        }
    }

    has_digit
}

fn escape_string(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(c),
        }
    }
    escaped
}
