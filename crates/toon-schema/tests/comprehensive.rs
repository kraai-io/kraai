use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Collection types test",
    example = r#"{"items":["a","b","c"],"maybe":null,"maybe_present":"present","required":42}"#,
    example = r#"{"items":[],"required":7}"#
)]
struct Collections {
    #[toon_schema(description = "List of strings")]
    items: Vec<String>,

    #[toon_schema(description = "Optional value")]
    maybe: Option<String>,

    #[toon_schema(description = "Optional with value")]
    maybe_present: Option<String>,

    #[toon_schema(description = "Required field")]
    required: i32,
}

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "All primitive types",
    example = r#"{"s":"string","i":42,"i64_val":42,"f":3.14,"f64_val":3.14,"b":true}"#
)]
struct AllTypes {
    #[toon_schema(description = "String")]
    s: String,
    #[toon_schema(description = "Integer")]
    i: i32,
    #[toon_schema(description = "Large integer")]
    i64_val: i64,
    #[toon_schema(description = "Float")]
    f: f32,
    #[toon_schema(description = "Large float")]
    f64_val: f64,
    #[toon_schema(description = "Bool")]
    b: bool,
}

#[test]
fn test_collections_structure() {
    let schema = Collections::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();
    let example_idx = lines.iter().position(|line| *line == "Examples:").unwrap();
    let schema_lines = &lines[..example_idx];

    assert!(schema_lines.contains(&"items[0:]: array<string>"));
    assert!(schema_lines.contains(&"maybe[0:1]: string"));
    assert!(schema_lines.contains(&"maybe_present[0:1]: string"));
    assert!(schema_lines.contains(&"required[1:1]: integer"));
}

#[test]
fn test_collections_examples_render_multiple_blocks() {
    let schema = Collections::toon_schema();
    assert_eq!(schema.matches("<tool_call>").count(), 2);
    assert!(schema.contains("Examples:\n<tool_call>"));
    assert!(schema.contains("items[3]: a,b,c"));
    assert!(schema.contains("maybe: null"));
    assert!(schema.contains("maybe_present: present"));
    assert!(
        schema.contains("\n\n<tool_call>\ntool: Collections\nitems[0]:\nrequired: 7\n</tool_call>")
    );
}

#[test]
fn test_primitive_type_mappings() {
    let schema = AllTypes::toon_schema();
    let field_lines: Vec<&str> = schema
        .lines()
        .filter(|line| line.contains(": ") && line.contains('['))
        .collect();

    assert!(field_lines.contains(&"s[1:1]: string"));
    assert!(field_lines.contains(&"i[1:1]: integer"));
    assert!(field_lines.contains(&"i64_val[1:1]: integer"));
    assert!(field_lines.contains(&"f[1:1]: float"));
    assert!(field_lines.contains(&"f64_val[1:1]: float"));
    assert!(field_lines.contains(&"b[1:1]: boolean"));
}

#[test]
fn test_no_trailing_whitespace_and_consistent_newlines() {
    let schema = Collections::toon_schema();
    for line in schema.lines() {
        assert_eq!(line.trim_end(), line);
    }

    let lines: Vec<&str> = schema.lines().collect();
    assert_eq!(schema, lines.join("\n"));
}
