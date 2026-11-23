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
