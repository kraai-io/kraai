//! Compile-time Toon format encoder for example values.
//!
//! This module converts JSON example values to Toon format at compile time,
//! producing a `&'static str` with zero runtime dependencies.
//!
//! # Toon Format String Quoting Rules
//!
//! Strings are only quoted when necessary per the Toon format specification:
//!
//! - Empty strings
//! - Leading/trailing whitespace
//! - Values matching `true`, `false`, or `null`
//! - Strings that look like numbers
//! - Strings containing `:`, `"`, `\`, `[`, `]`, `{`, `}`, `,`, or control characters
//! - Strings equal to `-` or starting with `-` followed by any character
//!
//! All other strings can be unquoted for token efficiency.

use crate::ir::{Field, PrimitiveType, Type};
use serde_json::Value;

/// Encode field examples to Toon format lines at compile time.
pub fn encode_example_toon(fields: &[Field]) -> syn::Result<Vec<String>> {
    let mut lines = Vec::new();

    for field in fields {
        if field.skipped {
            continue;
        }

        let example_value: Value = serde_json::from_str(&field.example).map_err(|e| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("Invalid JSON in example for field '{}': {}", field.name, e),
            )
        })?;

        let toon_value = value_to_toon(&field.name, &field.ty, &example_value, &field.range)?;
        lines.push(toon_value);
    }

    Ok(lines)
}

/// Convert a JSON value to Toon format string.
fn value_to_toon(
    name: &str,
    ty: &Type,
    value: &Value,
    range: &crate::ir::Range,
) -> syn::Result<String> {
    match (ty, value) {
        (Type::Primitive(PrimitiveType::String), Value::String(s)) => {
            let toon_str = toon_string(s);
            Ok(format!("{}: {}", name, toon_str))
        }
        (Type::Primitive(PrimitiveType::Integer), Value::Number(n)) => {
            Ok(format!("{}: {}", name, n))
        }
        (Type::Primitive(PrimitiveType::Float), Value::Number(n)) => Ok(format!("{}: {}", name, n)),
        (Type::Primitive(PrimitiveType::Boolean), Value::Bool(b)) => Ok(format!("{}: {}", name, b)),
        (Type::Array(inner), Value::Array(arr)) => {
            let count = arr.len();
            let range_str = format_array_range(range, count);
            let items: Vec<String> = arr
                .iter()
                .map(|v| array_item_to_toon(inner, v))
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(format!("{}{}: {}", name, range_str, items.join(",")))
        }
        (Type::Primitive(_), Value::Null) => Ok(format!("{}: null", name)),
        (Type::Array(_), Value::Null) => Ok(format!("{}[0]:", name)),
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "Type mismatch for field '{}': expected {:?}, got JSON value '{}'",
                name, ty, value
            ),
        )),
    }
}

/// Format array range notation for Toon format.
fn format_array_range(range: &crate::ir::Range, count: usize) -> String {
    match range {
        crate::ir::Range::ZeroOrMore => format!("[{}]", count),
        crate::ir::Range::AtLeast(min) => format!("[{}+]", count.min(*min as usize)),
        crate::ir::Range::Bounded(_, _) => format!("[{}]", count),
        _ => format!("[{}]", count),
    }
}

/// Convert an array item to Toon format.
fn array_item_to_toon(ty: &Type, value: &Value) -> syn::Result<String> {
    match (ty, value) {
        (Type::Primitive(PrimitiveType::String), Value::String(s)) => Ok(toon_string(s)),
        (Type::Primitive(PrimitiveType::Integer), Value::Number(n)) => Ok(n.to_string()),
        (Type::Primitive(PrimitiveType::Float), Value::Number(n)) => Ok(n.to_string()),
        (Type::Primitive(PrimitiveType::Boolean), Value::Bool(b)) => Ok(b.to_string()),
        (Type::Array(inner), Value::Array(arr)) => {
            let items: Vec<String> = arr
                .iter()
                .map(|v| array_item_to_toon(inner, v))
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

/// Convert a string to Toon format, quoting only when necessary.
///
/// A string must be quoted if:
/// - It's empty
/// - It has leading or trailing whitespace
/// - It equals "true", "false", or "null" (case-sensitive)
/// - It looks like a number
/// - It contains special characters: colon, quote, backslash, brackets, braces, control chars
/// - It contains the delimiter (comma in default context)
/// - It equals "-" or starts with "-" followed by any character
fn toon_string(s: &str) -> String {
    if needs_quoting(s) {
        format!("\"{}\"", escape_string(s))
    } else {
        s.to_string()
    }
}

/// Check if a string needs quoting in Toon format.
fn needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }

    if s != s.trim() {
        return true;
    }

    // Per Toon spec, quoting is case-sensitive: only exact lowercase matches
    if s == "true" || s == "false" || s == "null" {
        return true;
    }

    if looks_like_number(s) {
        return true;
    }

    // Per Toon spec: "It equals '-' or starts with '-' followed by any character"
    if s == "-" || (s.starts_with('-') && s.len() > 1) {
        return true;
    }

    for c in s.chars() {
        match c {
            ':' | '"' | '\\' | '[' | ']' | '{' | '}' | ',' | '\n' | '\r' | '\t' => return true,
            _ => {}
        }
    }

    false
}

/// Check if a string looks like a number (integer, float, or scientific notation).
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

/// Escape special characters in a string for Toon format.
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
