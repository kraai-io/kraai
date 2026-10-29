use std::fmt::Write;

use super::OutputValue;

const INLINE_ARRAY_LIMIT: usize = 100;
const INDENT: &str = "  ";

pub(crate) fn render(value: &OutputValue) -> String {
    let mut out = String::new();
    match value {
        OutputValue::Record(fields) if fields.is_empty() => out.push_str("{}"),
        OutputValue::Record(fields) => record(&mut out, fields, 0),
        OutputValue::List(items) => return list(items, items.len()),
        value => primitive(&mut out, value),
    }
    out
}

pub(crate) fn list(items: &[OutputValue], len: usize) -> String {
    let mut out = String::new();
    array(&mut out, None, items, len, 0);
    out
}

pub(crate) fn min_len(value: &OutputValue) -> usize {
    match value {
        OutputValue::Text(text) | OutputValue::Literal(text) => text.len(),
        OutputValue::List(items) => items.iter().map(min_len).sum(),
        OutputValue::Record(fields) => fields.iter().map(|(_, value)| min_len(value)).sum(),
        _ => 1,
    }
}

fn record(out: &mut String, fields: &[(String, OutputValue)], depth: usize) {
    for (index, (name, value)) in fields.iter().enumerate() {
        if index != 0 {
            out.push('\n');
        }
        indent(out, depth);
        match value {
            OutputValue::Record(nested) => {
                key(out, name);
                out.push(':');
                if !nested.is_empty() {
                    out.push('\n');
                    record(out, nested, depth + 1);
                }
            }
            OutputValue::List(items) => array(out, Some(name), items, items.len(), depth),
            value => {
                key(out, name);
                out.push_str(": ");
                primitive(out, value);
            }
        }
    }
}

fn array(out: &mut String, name: Option<&str>, items: &[OutputValue], len: usize, depth: usize) {
    if let Some(name) = name {
        key(out, name);
    }
    let _ = write!(out, "[{len}]");
    if items.is_empty() {
        out.push(':');
        return;
    }
    if items.iter().all(is_primitive) {
        let mut inline = String::new();
        for (index, item) in items.iter().enumerate() {
            if index != 0 {
                inline.push(',');
            }
            primitive(&mut inline, item);
        }
        if !inline.contains('\n') && inline.len() <= INLINE_ARRAY_LIMIT {
            out.push_str(": ");
            out.push_str(&inline);
            return;
        }
    } else if let Some(columns) = table_columns(items) {
        out.push('{');
        for (index, column) in columns.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            key(out, column);
        }
        out.push_str("}:");
        for item in items {
            out.push('\n');
            indent(out, depth + 1);
            if let OutputValue::Record(fields) = item {
                for (index, (_, value)) in fields.iter().enumerate() {
                    if index != 0 {
                        out.push(',');
                    }
                    primitive(out, value);
                }
            }
        }
        return;
    }
    out.push(':');
    for item in items {
        out.push('\n');
        list_item(out, item, depth + 1);
    }
}

fn list_item(out: &mut String, item: &OutputValue, depth: usize) {
    let mut nested = String::new();
    match item {
        OutputValue::Record(fields) if fields.is_empty() => {}
        OutputValue::Record(fields) => record(&mut nested, fields, depth + 1),
        OutputValue::List(items) => {
            indent(&mut nested, depth + 1);
            array(&mut nested, None, items, items.len(), depth + 1);
        }
        value => {
            indent(&mut nested, depth + 1);
            primitive(&mut nested, value);
        }
    }
    indent(out, depth);
    out.push('-');
    if let Some(rest) = nested.get(INDENT.len() * (depth + 1)..) {
        out.push(' ');
        out.push_str(rest);
    }
}

fn table_columns(items: &[OutputValue]) -> Option<Vec<&str>> {
    let Some(OutputValue::Record(first)) = items.first() else {
        return None;
    };
    if first.is_empty() {
        return None;
    }
    let columns: Vec<&str> = first.iter().map(|(name, _)| name.as_str()).collect();
    items
        .iter()
        .all(|item| {
            matches!(item, OutputValue::Record(fields)
            if fields.len() == columns.len()
                && fields.iter().zip(&columns).all(|((name, value), column)| {
                    name == column && is_primitive(value)
                }))
        })
        .then_some(columns)
}

fn is_primitive(value: &OutputValue) -> bool {
    !matches!(value, OutputValue::List(_) | OutputValue::Record(_))
}

fn primitive(out: &mut String, value: &OutputValue) {
    match value {
        OutputValue::Nothing => out.push_str("null"),
        OutputValue::Bool(value) => {
            let _ = write!(out, "{value}");
        }
        OutputValue::Int(value) => {
            let _ = write!(out, "{value}");
        }
        OutputValue::Float(value) if value.is_finite() => {
            let _ = write!(out, "{value}");
        }
        OutputValue::Float(_) => out.push_str("null"),
        OutputValue::Text(text) => string(out, text),
        OutputValue::Literal(text) if text.contains([',', '"', '\\', '\n', '\r']) => {
            quoted(out, text);
        }
        OutputValue::Literal(text) => out.push_str(text),
        OutputValue::Binary(bytes) if bytes.len() <= 32 => {
            out.push_str("0x[");
            for (index, byte) in bytes.iter().enumerate() {
                if index != 0 {
                    out.push(' ');
                }
                let _ = write!(out, "{byte:02x}");
            }
            out.push(']');
        }
        OutputValue::Binary(bytes) => {
            let _ = write!(out, "<binary {} bytes>", bytes.len());
        }
        OutputValue::List(_) | OutputValue::Record(_) => {}
    }
}

fn string(out: &mut String, text: &str) {
    if needs_quotes(text) {
        quoted(out, text);
    } else {
        out.push_str(text);
    }
}

fn needs_quotes(text: &str) -> bool {
    text.is_empty()
        || text.trim() != text
        || matches!(text, "true" | "false" | "null")
        || text.starts_with('-')
        || looks_numeric(text)
        || text.chars().any(|character| {
            matches!(character, ',' | ':' | '"' | '\\' | '[' | ']' | '{' | '}')
                || character.is_control()
        })
}

fn looks_numeric(text: &str) -> bool {
    text.parse::<f64>().is_ok()
        || (text.len() > 1
            && text.starts_with('0')
            && text.bytes().all(|byte| byte.is_ascii_digit()))
}

fn key(out: &mut String, name: &str) {
    let mut characters = name.chars();
    let bare = characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.'));
    if bare {
        out.push_str(name);
    } else {
        quoted(out, name);
    }
}

fn quoted(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(character));
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

#[cfg(test)]
mod tests {
    use super::OutputValue::{self, Bool, Int, List, Literal, Nothing, Record, Text};
    use super::render;

    fn record(fields: &[(&str, OutputValue)]) -> OutputValue {
        Record(
            fields
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        )
    }

    fn text(value: &str) -> OutputValue {
        Text(value.to_owned())
    }

    #[test]
    fn uniform_records_render_as_one_table() {
        let rows = List(vec![
            record(&[("name", text("a.rs")), ("size", Literal("1.2 kB".into()))]),
            record(&[("name", text("b c.rs")), ("size", Literal("10 B".into()))]),
        ]);
        assert_eq!(
            render(&rows),
            "[2]{name,size}:\n  a.rs,1.2 kB\n  b c.rs,10 B"
        );
    }

    #[test]
    fn records_render_as_indented_fields() {
        let value = record(&[
            ("stdout", text("line one\nline two")),
            ("exit_code", Int(7)),
            (
                "nested",
                record(&[
                    ("ok", Bool(true)),
                    ("tags", List(vec![text("a"), text("b")])),
                ]),
            ),
            ("empty", record(&[])),
            ("none", List(vec![])),
        ]);
        assert_eq!(
            render(&value),
            "stdout: \"line one\\nline two\"\nexit_code: 7\nnested:\n  ok: true\n  tags[2]: a,b\nempty:\nnone[0]:"
        );
    }

    #[test]
    fn mixed_lists_use_hyphen_items_aligned_with_their_fields() {
        let value = List(vec![
            Int(1),
            record(&[("value", Int(2)), ("nested", List(vec![text("x")]))]),
            List(vec![Int(3), Int(4)]),
            record(&[]),
            Nothing,
        ]);
        assert_eq!(
            render(&value),
            "[5]:\n  - 1\n  - value: 2\n    nested[1]: x\n  - [2]: 3,4\n  -\n  - null"
        );
    }

    #[test]
    fn records_with_nested_values_fall_back_to_list_items() {
        let value = List(vec![
            record(&[("value", Int(1)), ("nested", List(vec![text("a")]))]),
            record(&[("value", Int(2)), ("nested", List(vec![text("a")]))]),
        ]);
        assert_eq!(
            render(&value),
            "[2]:\n  - value: 1\n    nested[1]: a\n  - value: 2\n    nested[1]: a"
        );
    }

    #[test]
    fn long_primitive_lists_put_one_item_per_line() {
        let item = "x".repeat(60);
        let value = List(vec![text(&item), text(&item)]);
        assert_eq!(render(&value), format!("[2]:\n  - {item}\n  - {item}"));
    }

    #[test]
    fn ambiguous_strings_and_keys_are_quoted() {
        let value = record(&[
            (
                "plain",
                List(vec![
                    text(""),
                    text("42"),
                    text("true"),
                    text("-flag"),
                    text("a,b"),
                    text("https://x"),
                    text(" padded"),
                    text("007"),
                    text("fine words"),
                ]),
            ),
            ("needs space", Int(1)),
        ]);
        assert_eq!(
            render(&value),
            "plain[9]: \"\",\"42\",\"true\",\"-flag\",\"a,b\",\"https://x\",\" padded\",\"007\",fine words\n\"needs space\": 1"
        );
    }

    #[test]
    fn scalars_render_bare() {
        assert_eq!(render(&Int(4)), "4");
        assert_eq!(render(&OutputValue::Float(1.5)), "1.5");
        assert_eq!(render(&OutputValue::Binary(vec![0x41, 0x42])), "0x[41 42]");
        assert_eq!(render(&record(&[])), "{}");
        assert_eq!(render(&List(vec![])), "[0]:");
    }
}
