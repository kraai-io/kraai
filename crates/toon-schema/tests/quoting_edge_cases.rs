//! Tests for Toon format string quoting edge cases.
//!
//! Per the Toon format specification, strings are only quoted when necessary.

use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

// ============================================================================
// Case-Sensitive true/false/null Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Case-sensitive keyword tests")]
struct CaseSensitiveKeywords {
    #[toon_schema(example = "\"true\"")]
    lower_true: String,

    #[toon_schema(example = "\"false\"")]
    lower_false: String,

    #[toon_schema(example = "\"null\"")]
    lower_null: String,

    // These should NOT be quoted (case-sensitive per spec)
    #[toon_schema(example = "\"TRUE\"")]
    upper_true: String,

    #[toon_schema(example = "\"False\"")]
    mixed_false: String,

    #[toon_schema(example = "\"NULL\"")]
    upper_null: String,
}

#[test]
fn test_case_sensitive_keywords() {
    let schema = CaseSensitiveKeywords::toon_schema();
    let example_section = schema.split("Example:").nth(1).unwrap();

    // Lowercase keywords MUST be quoted
    assert!(
        example_section.contains("lower_true: \"true\""),
        "lowercase 'true' should be quoted"
    );
    assert!(
        example_section.contains("lower_false: \"false\""),
        "lowercase 'false' should be quoted"
    );
    assert!(
        example_section.contains("lower_null: \"null\""),
        "lowercase 'null' should be quoted"
    );

    // Non-lowercase should NOT be quoted (case-sensitive per Toon spec)
    assert!(
        example_section.contains("upper_true: TRUE"),
        "uppercase 'TRUE' should NOT be quoted"
    );
    assert!(
        example_section.contains("mixed_false: False"),
        "mixed case 'False' should NOT be quoted"
    );
    assert!(
        example_section.contains("upper_null: NULL"),
        "uppercase 'NULL' should NOT be quoted"
    );
}

// ============================================================================
// Dash Quoting Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Dash quoting tests")]
struct DashQuoting {
    // Single dash must be quoted
    #[toon_schema(example = "\"-\"")]
    just_dash: String,

    // Dash followed by any character must be quoted
    #[toon_schema(example = "\"-test\"")]
    dash_test: String,

    #[toon_schema(example = "\"-hello\"")]
    dash_hello: String,

    #[toon_schema(example = "\"-x\"")]
    dash_x: String,

    // Dash in middle should NOT be quoted
    #[toon_schema(example = "\"hello-world\"")]
    dash_in_middle: String,

    // Dash followed by space must be quoted
    #[toon_schema(example = "\"- test\"")]
    dash_space: String,
}

#[test]
fn test_dash_quoting() {
    let schema = DashQuoting::toon_schema();
    let example_section = schema.split("Example:").nth(1).unwrap();

    // All dash-prefixed strings must be quoted
    assert!(
        example_section.contains("just_dash: \"-\""),
        "single dash should be quoted"
    );
    assert!(
        example_section.contains("dash_test: \"-test\""),
        "-test should be quoted"
    );
    assert!(
        example_section.contains("dash_hello: \"-hello\""),
        "-hello should be quoted"
    );
    assert!(
        example_section.contains("dash_x: \"-x\""),
        "-x should be quoted"
    );
    assert!(
        example_section.contains("dash_space: \"- test\""),
        "- test should be quoted"
    );

    // Dash in middle should NOT be quoted
    assert!(
        example_section.contains("dash_in_middle: hello-world"),
        "hello-world should NOT be quoted"
    );
}

// ============================================================================
// Number-Like Strings Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Number-like string tests")]
struct NumberLikeStrings {
    #[toon_schema(example = "\"42\"")]
    looks_like_int: String,

    #[toon_schema(example = "\"-3.14\"")]
    looks_like_float: String,

    #[toon_schema(example = "\"1e6\"")]
    looks_like_exp: String,

    #[toon_schema(example = "\"hello42world\"")]
    contains_number: String,
}

#[test]
fn test_number_like_strings() {
    let schema = NumberLikeStrings::toon_schema();
    let example_section = schema.split("Example:").nth(1).unwrap();

    // Number-like strings must be quoted
    assert!(
        example_section.contains("looks_like_int: \"42\""),
        "42 should be quoted"
    );
    assert!(
        example_section.contains("looks_like_float: \"-3.14\""),
        "-3.14 should be quoted"
    );
    assert!(
        example_section.contains("looks_like_exp: \"1e6\""),
        "1e6 should be quoted"
    );

    // String containing numbers but not number-like should NOT be quoted
    assert!(
        example_section.contains("contains_number: hello42world"),
        "hello42world should NOT be quoted"
    );
}

// ============================================================================
// Special Character Strings Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Special character string tests")]
struct SpecialCharStrings {
    #[toon_schema(example = "\"key:value\"")]
    with_colon: String,

    #[toon_schema(example = "\"a, b, c\"")]
    with_comma: String,

    #[toon_schema(example = "\"say \\\"hello\\\"\"")]
    with_quote: String,

    #[toon_schema(example = "\"path\\\\to\\\\file\"")]
    with_backslash: String,
}

#[test]
fn test_special_char_strings() {
    let schema = SpecialCharStrings::toon_schema();
    let example_section = schema.split("Example:").nth(1).unwrap();

    // Strings with special characters must be quoted
    assert!(
        example_section.contains("with_colon: \"key:value\""),
        "key:value should be quoted"
    );
    assert!(
        example_section.contains("with_comma: \"a, b, c\""),
        "a, b, c should be quoted"
    );
    assert!(
        example_section.contains("with_quote: \"say \\\"hello\\\"\""),
        "string with quote should be quoted and escaped"
    );
    assert!(
        example_section.contains("with_backslash: \"path\\\\to\\\\file\""),
        "string with backslash should be quoted and escaped"
    );
}

// ============================================================================
// Regular Strings Tests (should NOT be quoted)
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Regular string tests")]
struct RegularStrings {
    #[toon_schema(example = "\"hello\"")]
    simple: String,

    #[toon_schema(example = "\"Hello World\"")]
    with_space: String,

    #[toon_schema(example = "\"hello_world\"")]
    with_underscore: String,

    #[toon_schema(example = "\"path/to/file\"")]
    with_slash: String,
}

#[test]
fn test_regular_strings_not_quoted() {
    let schema = RegularStrings::toon_schema();
    let example_section = schema.split("Example:").nth(1).unwrap();

    // Regular strings should NOT be quoted
    assert!(
        example_section.contains("simple: hello"),
        "hello should NOT be quoted"
    );
    assert!(
        example_section.contains("with_space: Hello World"),
        "Hello World should NOT be quoted"
    );
    assert!(
        example_section.contains("with_underscore: hello_world"),
        "hello_world should NOT be quoted"
    );
    assert!(
        example_section.contains("with_slash: path/to/file"),
        "path/to/file should NOT be quoted"
    );
}
