//! Tests for new features: custom ranges, default values, reserved words

use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

// ============================================================================
// Custom Range Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Custom ranges test")]
struct CustomRanges {
    #[toon_schema(description = "At least one", example = "[\"a\"]", min = 1)]
    at_least_one: Vec<String>,

    #[toon_schema(description = "Bounded", example = "[\"a\", \"b\"]", min = 1, max = 5)]
    bounded: Vec<String>,

    #[toon_schema(description = "Max only", example = "[]", max = 3)]
    max_only: Vec<String>,

    #[toon_schema(description = "Standard vec", example = "[]")]
    standard_vec: Vec<String>,
}

#[test]
fn test_custom_ranges() {
    let schema = CustomRanges::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    let example_idx = lines.iter().position(|l| l == &"Example:").unwrap();
    let schema_lines = &lines[0..example_idx];

    // Find each field and verify its range
    let at_least_line = schema_lines
        .iter()
        .find(|l| l.starts_with("at_least_one"))
        .unwrap();
    assert_eq!(
        *at_least_line, "at_least_one[1:]: array<string>",
        "Should have [1:] range for min=1: {}",
        at_least_line
    );

    let bounded_line = schema_lines
        .iter()
        .find(|l| l.starts_with("bounded"))
        .unwrap();
    assert_eq!(
        *bounded_line, "bounded[1:5]: array<string>",
        "Should have [1:5] range for min=1,max=5: {}",
        bounded_line
    );

    let max_line = schema_lines
        .iter()
        .find(|l| l.starts_with("max_only"))
        .unwrap();
    assert_eq!(
        *max_line, "max_only[0:3]: array<string>",
        "Should have [0:3] range for max=3: {}",
        max_line
    );

    let standard_line = schema_lines
        .iter()
        .find(|l| l.starts_with("standard_vec"))
        .unwrap();
    assert_eq!(
        *standard_line, "standard_vec[0:]: array<string>",
        "Should have [0:] range for no custom range: {}",
        standard_line
    );
}

// ============================================================================
// Default Value Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Default values test")]
struct WithDefaults {
    #[toon_schema(description = "With default", example = "\"hello\"")]
    #[serde(default = "default_greeting")]
    greeting: String,

    #[toon_schema(description = "With default bool", example = "true")]
    #[serde(default)]
    enabled: bool,

    #[toon_schema(description = "No default", example = "42")]
    required: i32,
}

fn default_greeting() -> String {
    "world".to_string()
}

#[test]
fn test_default_values() {
    let schema = WithDefaults::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    let example_idx = lines.iter().position(|l| l == &"Example:").unwrap();
    let schema_lines = &lines[0..example_idx];

    // Check that defaults are shown as comments
    let greeting_line = schema_lines
        .iter()
        .find(|l| l.starts_with("greeting"))
        .unwrap();
    assert!(
        greeting_line.contains("# default:"),
        "Should show default as comment: {}",
        greeting_line
    );

    let enabled_line = schema_lines
        .iter()
        .find(|l| l.starts_with("enabled"))
        .unwrap();
    assert!(
        enabled_line.contains("# default:"),
        "Should show default as comment: {}",
        enabled_line
    );

    // Check that non-default field has no default comment
    let required_line = schema_lines
        .iter()
        .find(|l| l.starts_with("required"))
        .unwrap();
    assert!(
        !required_line.contains("# default:"),
        "Should not show default for required field: {}",
        required_line
    );
}

// ============================================================================
// Compile-Time Schema Test
// ============================================================================

#[test]
fn test_compile_time_schema() {
    // Verify that toon_schema() returns &'static str (compile-time constant)
    let schema: &'static str = CustomRanges::toon_schema();
    assert!(!schema.is_empty());

    // Verify we can call it multiple times and get the same result
    let schema2 = CustomRanges::toon_schema();
    assert_eq!(schema, schema2);
}
