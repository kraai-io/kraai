//! Tests to verify that the compile-time example generation in toon-schema
//! produces the exact same output as the runtime toon-format crate.
//!
//! This ensures our manual compile-time implementation matches the external
//! runtime toon-format encoder.

use serde::{Deserialize, Serialize};
use serde_json::json;
use toon_schema::ToonSchema;

fn extract_example_lines(schema: &str) -> Vec<String> {
    let lines: Vec<&str> = schema.lines().collect();
    let tool_call_start = lines
        .iter()
        .position(|l| *l == "<tool_call>")
        .expect("Should have tool_call block");
    let tool_call_end = lines
        .iter()
        .position(|l| *l == "</tool_call>")
        .expect("Should have closing </tool_call>");

    lines[tool_call_start + 1..tool_call_end]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn compare_toon_output(json: &serde_json::Value, schema: &str) {
    let expected = toon_format::encode_default(json).unwrap();
    let expected_lines: Vec<&str> = expected.lines().collect();

    let actual_lines = extract_example_lines(schema);

    // Skip the "tool: <name>" line from actual output (toon-format doesn't include it)
    let actual_field_lines: Vec<&str> = actual_lines
        .iter()
        .filter(|l| !l.starts_with("tool:"))
        .map(|s| s.as_str())
        .collect();

    assert_eq!(
        expected_lines.len(),
        actual_field_lines.len(),
        "Line count mismatch.\nExpected (toon-format):\n{}\n\nActual (toon-schema):\n{}",
        expected,
        actual_field_lines.join("\n")
    );

    for (i, (expected_line, actual_line)) in expected_lines
        .iter()
        .zip(actual_field_lines.iter())
        .enumerate()
    {
        assert_eq!(
            expected_line, actual_line,
            "Line {} mismatch.\nExpected: {:?}\nActual:   {:?}",
            i, expected_line, actual_line
        );
    }
}

// ============================================================================
// Basic Types
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Basic types test", name = "basic_types")]
struct BasicTypes {
    #[toon_schema(description = "A string field", example = "\"hello\"")]
    s: String,

    #[toon_schema(description = "An integer field", example = "42")]
    i: i32,

    #[toon_schema(description = "A boolean field", example = "true")]
    b: bool,
}

#[test]
fn test_basic_types_match_toon_format() {
    let json = json!({
        "s": "hello",
        "i": 42,
        "b": true
    });

    let schema = BasicTypes::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// String Quoting - Keywords
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Keyword edge cases", name = "keyword_edge_cases")]
struct KeywordEdgeCases {
    #[toon_schema(description = "Lowercase true", example = "\"true\"")]
    lower_true: String,

    #[toon_schema(description = "Lowercase false", example = "\"false\"")]
    lower_false: String,

    #[toon_schema(description = "Lowercase null", example = "\"null\"")]
    lower_null: String,

    #[toon_schema(description = "Uppercase TRUE", example = "\"TRUE\"")]
    upper_true: String,

    #[toon_schema(description = "Uppercase FALSE", example = "\"FALSE\"")]
    upper_false: String,

    #[toon_schema(description = "Uppercase NULL", example = "\"NULL\"")]
    upper_null: String,

    #[toon_schema(description = "Mixed True", example = "\"True\"")]
    mixed_true: String,

    #[toon_schema(description = "Mixed False", example = "\"False\"")]
    mixed_false: String,

    #[toon_schema(description = "Mixed Null", example = "\"Null\"")]
    mixed_null: String,
}

#[test]
fn test_keyword_edge_cases() {
    let json = json!({
        "lower_true": "true",
        "lower_false": "false",
        "lower_null": "null",
        "upper_true": "TRUE",
        "upper_false": "FALSE",
        "upper_null": "NULL",
        "mixed_true": "True",
        "mixed_false": "False",
        "mixed_null": "Null"
    });

    let schema = KeywordEdgeCases::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// String Quoting - Numbers
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Number-like strings", name = "number_like_strings")]
struct NumberLikeStrings {
    #[toon_schema(description = "Integer string", example = "\"42\"")]
    int_str: String,

    #[toon_schema(description = "Negative integer string", example = "\"-17\"")]
    neg_int_str: String,

    #[toon_schema(description = "Float string", example = "\"3.14\"")]
    float_str: String,

    #[toon_schema(description = "Scientific notation", example = "\"1e10\"")]
    sci_str: String,

    #[toon_schema(description = "Scientific with decimal", example = "\"1.5e-3\"")]
    sci_decimal_str: String,

    #[toon_schema(description = "Zero", example = "\"0\"")]
    zero_str: String,

    #[toon_schema(description = "Negative zero", example = "\"-0\"")]
    neg_zero_str: String,

    #[toon_schema(description = "Leading zero", example = "\"05\"")]
    leading_zero_str: String,

    #[toon_schema(description = "Double leading zero", example = "\"007\"")]
    double_leading_zero: String,
}

#[test]
fn test_number_like_strings() {
    let json = json!({
        "int_str": "42",
        "neg_int_str": "-17",
        "float_str": "3.14",
        "sci_str": "1e10",
        "sci_decimal_str": "1.5e-3",
        "zero_str": "0",
        "neg_zero_str": "-0",
        "leading_zero_str": "05",
        "double_leading_zero": "007"
    });

    let schema = NumberLikeStrings::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// String Quoting - Whitespace
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Whitespace edge cases", name = "whitespace_edge_cases")]
struct WhitespaceEdgeCases {
    #[toon_schema(description = "Leading space", example = "\" hello\"")]
    leading_space: String,

    #[toon_schema(description = "Trailing space", example = "\"hello \"")]
    trailing_space: String,

    #[toon_schema(description = "Leading and trailing", example = "\" hello \"")]
    both_spaces: String,

    #[toon_schema(description = "Internal spaces", example = "\"hello world\"")]
    internal_spaces: String,

    #[toon_schema(description = "Multiple spaces", example = "\"hello  world\"")]
    multiple_spaces: String,

    #[toon_schema(description = "Tab character", example = "\"hello\\tworld\"")]
    with_tab: String,

    #[toon_schema(description = "Carriage return", example = "\"hello\\rworld\"")]
    with_cr: String,
}

#[test]
fn test_whitespace_edge_cases() {
    let json = json!({
        "leading_space": " hello",
        "trailing_space": "hello ",
        "both_spaces": " hello ",
        "internal_spaces": "hello world",
        "multiple_spaces": "hello  world",
        "with_tab": "hello\tworld",
        "with_cr": "hello\rworld"
    });

    let schema = WhitespaceEdgeCases::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// String Quoting - Structural Characters
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Structural character edge cases",
    name = "structural_chars"
)]
struct StructuralCharEdgeCases {
    #[toon_schema(description = "With colon", example = "\"key:value\"")]
    with_colon: String,

    #[toon_schema(description = "With bracket open", example = "\"hello[world\"")]
    with_bracket_open: String,

    #[toon_schema(description = "With bracket close", example = "\"hello]world\"")]
    with_bracket_close: String,

    #[toon_schema(description = "With brace open", example = "\"hello{world\"")]
    with_brace_open: String,

    #[toon_schema(description = "With brace close", example = "\"hello}world\"")]
    with_brace_close: String,

    #[toon_schema(description = "With comma", example = "\"a,b\"")]
    with_comma: String,

    #[toon_schema(description = "With dash", example = "\"hello-world\"")]
    with_dash: String,

    #[toon_schema(description = "Multiple dashes", example = "\"hello--world\"")]
    with_multiple_dashes: String,

    #[toon_schema(description = "Ends with dash", example = "\"hello-\"")]
    ends_with_dash: String,

    #[toon_schema(description = "Single dash", example = "\"-\"")]
    single_dash: String,
}

#[test]
fn test_structural_char_edge_cases() {
    let json = json!({
        "with_colon": "key:value",
        "with_bracket_open": "hello[world",
        "with_bracket_close": "hello]world",
        "with_brace_open": "hello{world",
        "with_brace_close": "hello}world",
        "with_comma": "a,b",
        "with_dash": "hello-world",
        "with_multiple_dashes": "hello--world",
        "ends_with_dash": "hello-",
        "single_dash": "-"
    });

    let schema = StructuralCharEdgeCases::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// String Quoting - Escape Sequences
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Escape sequence edge cases", name = "escape_sequences")]
struct EscapeSequenceEdgeCases {
    #[toon_schema(description = "With quote", example = "\"say \\\"hello\\\"\"")]
    with_quote: String,

    #[toon_schema(description = "With backslash", example = "\"path\\\\to\\\\file\"")]
    with_backslash: String,

    #[toon_schema(description = "With newline", example = "\"line1\\nline2\"")]
    with_newline: String,

    #[toon_schema(description = "With tab", example = "\"col1\\tcol2\"")]
    with_tab: String,

    #[toon_schema(description = "With carriage return", example = "\"line1\\rline2\"")]
    with_cr: String,

    #[toon_schema(description = "Mixed escapes", example = "\"a\\\"b\\\\c\\nd\"")]
    mixed_escapes: String,
}

#[test]
fn test_escape_sequence_edge_cases() {
    let json = json!({
        "with_quote": "say \"hello\"",
        "with_backslash": "path\\to\\file",
        "with_newline": "line1\nline2",
        "with_tab": "col1\tcol2",
        "with_cr": "line1\rline2",
        "mixed_escapes": "a\"b\\c\nd"
    });

    let schema = EscapeSequenceEdgeCases::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// Unicode and Special Strings
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Unicode edge cases", name = "unicode_edge_cases")]
struct UnicodeEdgeCases {
    #[toon_schema(description = "Emoji", example = "\"hello \\ud83d\\ude00\"")]
    emoji: String,

    #[toon_schema(
        description = "Chinese characters",
        example = "\"hello \\u4e16\\u754c\""
    )]
    chinese: String,

    #[toon_schema(
        description = "Japanese",
        example = "\"\\u3053\\u3093\\u306b\\u3061\\u306f\""
    )]
    japanese: String,

    #[toon_schema(description = "Accented chars", example = "\"caf\\u00e9\"")]
    accented: String,

    #[toon_schema(description = "Symbols", example = "\"\\u00a9 2024\"")]
    symbols: String,
}

#[test]
fn test_unicode_edge_cases() {
    let json = json!({
        "emoji": "hello 😀",
        "chinese": "hello 世界",
        "japanese": "こんにちは",
        "accented": "café",
        "symbols": "© 2024"
    });

    let schema = UnicodeEdgeCases::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// Empty and Single Values
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Empty and single values", name = "empty_single")]
struct EmptySingleValues {
    #[toon_schema(description = "Empty string", example = "\"\"")]
    empty_string: String,

    #[toon_schema(description = "Single char", example = "\"a\"")]
    single_char: String,

    #[toon_schema(description = "Single element array", example = "[\"only\"]")]
    single_elem_array: Vec<String>,

    #[toon_schema(description = "Empty array", example = "[]")]
    empty_array: Vec<String>,

    #[toon_schema(description = "Single int array", example = "[42]")]
    single_int_array: Vec<i32>,

    #[toon_schema(description = "Empty int array", example = "[]")]
    empty_int_array: Vec<i32>,
}

#[test]
fn test_empty_single_values() {
    let json = json!({
        "empty_string": "",
        "single_char": "a",
        "single_elem_array": ["only"],
        "empty_array": serde_json::json!([]),
        "single_int_array": [42],
        "empty_int_array": serde_json::json!([])
    });

    let schema = EmptySingleValues::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// Number Types
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Number edge cases", name = "number_edge_cases")]
struct NumberEdgeCases {
    #[toon_schema(description = "Zero", example = "0")]
    zero: i32,

    #[toon_schema(description = "Negative zero", example = "0")]
    neg_zero: i32,

    #[toon_schema(description = "Large integer", example = "9999999999")]
    large_int: i64,

    #[toon_schema(description = "Small negative", example = "-9999999999")]
    small_neg: i64,

    #[toon_schema(description = "Simple float", example = "3.14")]
    simple_float: f64,

    #[toon_schema(description = "Negative float", example = "-2.718")]
    neg_float: f64,

    #[toon_schema(description = "Small decimal float", example = "0.125")]
    small_float: f64,

    #[toon_schema(description = "Another float", example = "2.5")]
    another_float: f64,
}

#[test]
fn test_number_edge_cases() {
    let json = json!({
        "zero": 0,
        "neg_zero": 0,
        "large_int": 9999999999_i64,
        "small_neg": -9999999999_i64,
        "simple_float": 3.14,
        "neg_float": -2.718,
        "small_float": 0.125,
        "another_float": 2.5
    });

    let schema = NumberEdgeCases::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// Array Types
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Array edge cases", name = "array_edge_cases")]
struct ArrayEdgeCases {
    #[toon_schema(description = "String array", example = "[\"a\", \"b\", \"c\"]")]
    string_array: Vec<String>,

    #[toon_schema(description = "Integer array", example = "[1, 2, 3]")]
    int_array: Vec<i32>,

    #[toon_schema(description = "Boolean array", example = "[true, false, true]")]
    bool_array: Vec<bool>,

    #[toon_schema(description = "Float array", example = "[1.1, 2.2, 3.3]")]
    float_array: Vec<f64>,

    #[toon_schema(
        description = "Mixed content strings",
        example = "[\"hello world\", \"a,b\", \"test-1\"]"
    )]
    mixed_strings: Vec<String>,

    #[toon_schema(description = "Negative int array", example = "[-1, -2, -3]")]
    neg_int_array: Vec<i32>,
}

#[test]
fn test_array_edge_cases() {
    let json = json!({
        "string_array": ["a", "b", "c"],
        "int_array": [1, 2, 3],
        "bool_array": [true, false, true],
        "float_array": [1.1, 2.2, 3.3],
        "mixed_strings": ["hello world", "a,b", "test-1"],
        "neg_int_array": [-1, -2, -3]
    });

    let schema = ArrayEdgeCases::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// Boolean Edge Cases
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Boolean edge cases", name = "bool_edge_cases")]
struct BoolEdgeCases {
    #[toon_schema(description = "True value", example = "true")]
    true_val: bool,

    #[toon_schema(description = "False value", example = "false")]
    false_val: bool,
}

#[test]
fn test_bool_edge_cases() {
    let json = json!({
        "true_val": true,
        "false_val": false
    });

    let schema = BoolEdgeCases::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// Safe Unquoted Strings
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Safe unquoted strings", name = "safe_unquoted")]
struct SafeUnquotedStrings {
    #[toon_schema(description = "Simple word", example = "\"hello\"")]
    simple: String,

    #[toon_schema(description = "With underscore", example = "\"hello_world\"")]
    with_underscore: String,

    #[toon_schema(description = "With slash", example = "\"path/to/file\"")]
    with_slash: String,

    #[toon_schema(description = "With dot", example = "\"file.txt\"")]
    with_dot: String,

    #[toon_schema(description = "With at sign", example = "\"user@domain\"")]
    with_at: String,

    #[toon_schema(description = "With hash", example = "\"#hashtag\"")]
    with_hash: String,

    #[toon_schema(description = "With dollar", example = "\"$variable\"")]
    with_dollar: String,

    #[toon_schema(description = "With percent", example = "\"100%\"")]
    with_percent: String,

    #[toon_schema(description = "With ampersand", example = "\"a & b\"")]
    with_ampersand: String,

    #[toon_schema(description = "With asterisk", example = "\"*.txt\"")]
    with_asterisk: String,

    #[toon_schema(description = "With plus", example = "\"a+b\"")]
    with_plus: String,

    #[toon_schema(description = "With equals", example = "\"key=value\"")]
    with_equals: String,

    #[toon_schema(description = "With question", example = "\"query?\"")]
    with_question: String,

    #[toon_schema(description = "With exclamation", example = "\"hello!\"")]
    with_exclamation: String,

    #[toon_schema(description = "With semicolon", example = "\"a; b\"")]
    with_semicolon: String,

    #[toon_schema(description = "With angle brackets", example = "\"<tag>\"")]
    with_angle_brackets: String,

    #[toon_schema(description = "With parentheses", example = "\"func()\"")]
    with_parens: String,

    #[toon_schema(description = "With pipe", example = "\"a | b\"")]
    with_pipe: String,

    #[toon_schema(description = "With tilde", example = "\"~home\"")]
    with_tilde: String,

    #[toon_schema(description = "With backtick", example = "\"`code`\"")]
    with_backtick: String,

    #[toon_schema(description = "With single quote", example = "\"it's\"")]
    with_single_quote: String,
}

#[test]
fn test_safe_unquoted_strings() {
    let json = json!({
        "simple": "hello",
        "with_underscore": "hello_world",
        "with_slash": "path/to/file",
        "with_dot": "file.txt",
        "with_at": "user@domain",
        "with_hash": "#hashtag",
        "with_dollar": "$variable",
        "with_percent": "100%",
        "with_ampersand": "a & b",
        "with_asterisk": "*.txt",
        "with_plus": "a+b",
        "with_equals": "key=value",
        "with_question": "query?",
        "with_exclamation": "hello!",
        "with_semicolon": "a; b",
        "with_angle_brackets": "<tag>",
        "with_parens": "func()",
        "with_pipe": "a | b",
        "with_tilde": "~home",
        "with_backtick": "`code`",
        "with_single_quote": "it's"
    });

    let schema = SafeUnquotedStrings::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// Real-world Examples
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(name = "read_files", description = "Read files from the filesystem")]
struct ReadFilesArgs {
    #[toon_schema(
        description = "List of file paths",
        example = "[\"/etc/passwd\", \"/etc/hosts\"]"
    )]
    files: Vec<String>,

    #[toon_schema(description = "Encoding format", example = "\"utf-8\"")]
    encoding: String,

    #[toon_schema(description = "Max file size", example = "1048576")]
    max_size: i64,
}

#[test]
fn test_read_files_match_toon_format() {
    let json = json!({
        "files": ["/etc/passwd", "/etc/hosts"],
        "encoding": "utf-8",
        "max_size": 1048576
    });

    let schema = ReadFilesArgs::toon_schema();
    compare_toon_output(&json, &schema);
}

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "API request", name = "api_request")]
struct ApiRequest {
    #[toon_schema(
        description = "API endpoint",
        example = "\"https://api.example.com/v1/users\""
    )]
    url: String,

    #[toon_schema(description = "HTTP method", example = "\"POST\"")]
    method: String,

    #[toon_schema(
        description = "Request headers",
        example = "[\"Content-Type: application/json\", \"Authorization: Bearer token\"]"
    )]
    headers: Vec<String>,

    #[toon_schema(description = "Timeout in seconds", example = "30")]
    timeout: i32,

    #[toon_schema(description = "Follow redirects", example = "true")]
    follow_redirects: bool,

    #[toon_schema(description = "Retry count", example = "3")]
    retries: i32,
}

#[test]
fn test_api_request_match_toon_format() {
    let json = json!({
        "url": "https://api.example.com/v1/users",
        "method": "POST",
        "headers": ["Content-Type: application/json", "Authorization: Bearer token"],
        "timeout": 30,
        "follow_redirects": true,
        "retries": 3
    });

    let schema = ApiRequest::toon_schema();
    compare_toon_output(&json, &schema);
}

// ============================================================================
// Complex Mixed Cases
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Complex mixed types", name = "complex_mixed")]
struct ComplexMixedTypes {
    #[toon_schema(
        description = "String with many specials",
        example = "\"key: value, [array], {obj} - dash\""
    )]
    complex_string: String,

    #[toon_schema(
        description = "Array with quoted items",
        example = "[\"true\", \"false\", \"null\", \"123\"]"
    )]
    keyword_lookalikes: Vec<String>,

    #[toon_schema(
        description = "Array of paths",
        example = "[\"/usr/local/bin\", \"/home/user/.config\"]"
    )]
    paths: Vec<String>,

    #[toon_schema(description = "Flags", example = "[true, false, true, false]")]
    flags: Vec<bool>,

    #[toon_schema(description = "Numbers", example = "[0, -1, 100, 999999]")]
    numbers: Vec<i64>,
}

#[test]
fn test_complex_mixed_types() {
    let json = json!({
        "complex_string": "key: value, [array], {obj} - dash",
        "keyword_lookalikes": ["true", "false", "null", "123"],
        "paths": ["/usr/local/bin", "/home/user/.config"],
        "flags": [true, false, true, false],
        "numbers": [0, -1, 100, 999999]
    });

    let schema = ComplexMixedTypes::toon_schema();
    compare_toon_output(&json, &schema);
}
