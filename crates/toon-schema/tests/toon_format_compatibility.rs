use serde::{Deserialize, Serialize};
use serde_json::json;
use toon_schema::ToonSchema;

fn extract_first_example_lines(schema: &str) -> Vec<String> {
    let lines: Vec<&str> = schema.lines().collect();
    let tool_call_start = lines
        .iter()
        .position(|line| *line == "<tool_call>")
        .expect("tool call start");
    let tool_call_end = lines
        .iter()
        .position(|line| *line == "</tool_call>")
        .expect("tool call end");

    lines[tool_call_start + 1..tool_call_end]
        .iter()
        .map(|line| line.to_string())
        .collect()
}

fn compare_toon_output(json: &serde_json::Value, schema: &str) {
    let expected = toon_format::encode_default(json).unwrap();
    let expected_lines: Vec<&str> = expected.lines().collect();

    let actual_lines = extract_first_example_lines(schema);
    let actual_field_lines: Vec<&str> = actual_lines
        .iter()
        .filter(|line| !line.starts_with("tool:"))
        .map(|line| line.as_str())
        .collect();

    assert_eq!(expected_lines, actual_field_lines);
}

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Basic types test",
    name = "basic_types",
    example = r#"{"s":"hello","i":42,"b":true}"#,
    example = r#"{"s":"world","i":7,"b":false}"#
)]
struct BasicTypes {
    #[toon_schema(description = "A string field")]
    s: String,
    #[toon_schema(description = "An integer field")]
    i: i32,
    #[toon_schema(description = "A boolean field")]
    b: bool,
}

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Read files from the filesystem",
    name = "read_files",
    example = r#"{"files":["/etc/passwd","/etc/hosts"],"encoding":"utf-8","max_size":1048576}"#
)]
struct ReadFilesArgs {
    #[toon_schema(description = "Files to read", min = 1)]
    files: Vec<String>,
    #[toon_schema(description = "Encoding format")]
    encoding: Option<String>,
    #[toon_schema(description = "Max file size")]
    max_size: i64,
}

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Complex mixed types",
    name = "complex_mixed",
    example = r#"{"message":"key: value, [array], {obj} - dash","keywords":["true","false","null","123"],"paths":["/usr/local/bin","/home/user/.config"],"flags":[true,false,true,false],"numbers":[0,-1,100,999999]}"#
)]
struct ComplexMixedTypes {
    #[toon_schema(description = "Message")]
    message: String,
    #[toon_schema(description = "Keywords")]
    keywords: Vec<String>,
    #[toon_schema(description = "Paths")]
    paths: Vec<String>,
    #[toon_schema(description = "Flags")]
    flags: Vec<bool>,
    #[toon_schema(description = "Numbers")]
    numbers: Vec<i32>,
}

#[test]
fn test_basic_types_match_toon_format() {
    compare_toon_output(&json!({"s":"hello","i":42,"b":true}), BasicTypes::toon_schema());
    assert_eq!(BasicTypes::toon_schema().matches("<tool_call>").count(), 2);
}

#[test]
fn test_read_files_match_toon_format() {
    compare_toon_output(
        &json!({"files":["/etc/passwd","/etc/hosts"],"encoding":"utf-8","max_size":1048576}),
        ReadFilesArgs::toon_schema(),
    );
}

#[test]
fn test_complex_mixed_types_match_toon_format() {
    compare_toon_output(
        &json!({
            "message": "key: value, [array], {obj} - dash",
            "keywords": ["true", "false", "null", "123"],
            "paths": ["/usr/local/bin", "/home/user/.config"],
            "flags": [true, false, true, false],
            "numbers": [0, -1, 100, 999999]
        }),
        ComplexMixedTypes::toon_schema(),
    );
}
