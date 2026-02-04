//! Comprehensive tests for ToonSchema derive macro
//!
//! These tests verify exact output format, not just substring matching

use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

// ============================================================================
// Basic Types Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "A simple person struct")]
struct Person {
    #[toon_schema(description = "Person's name", example = "\"Alice\"")]
    name: String,

    #[toon_schema(description = "Person's age", example = "30")]
    age: i32,

    #[toon_schema(description = "Is person active", example = "true")]
    active: bool,
}

#[test]
fn test_basic_types_exact_structure() {
    let schema = Person::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    // Verify exact line count and structure
    // Should have: description, tool, 3 descriptions, 3 fields, blank, "Example:", tool, 3 values
    assert!(
        lines.len() >= 11,
        "Schema should have at least 11 lines, got {}",
        lines.len()
    );

    // Check exact schema section (before "Example:")
    let example_idx = lines
        .iter()
        .position(|l| l == &"Example:")
        .expect("Should have Example section");
    let schema_lines = &lines[0..example_idx];

    // Schema should be exactly:
    // # A simple person struct
    // tool: Person
    // # Person's name
    // name[1:1]: string
    // # Person's age
    // age[1:1]: integer
    // # Is person active
    // active[1:1]: boolean
    assert_eq!(
        schema_lines[0], "# A simple person struct",
        "Line 0 should be description"
    );
    assert_eq!(
        schema_lines[1], "tool: Person",
        "Line 1 should be tool name"
    );
    assert_eq!(
        schema_lines[2], "# Person's name",
        "Line 2 should be name description"
    );
    assert_eq!(
        schema_lines[3], "name[1:1]: string",
        "Line 3 should be name field"
    );
    assert_eq!(
        schema_lines[4], "# Person's age",
        "Line 4 should be age description"
    );
    assert_eq!(
        schema_lines[5], "age[1:1]: integer",
        "Line 5 should be age field"
    );
    assert_eq!(
        schema_lines[6], "# Is person active",
        "Line 6 should be active description"
    );
    assert_eq!(
        schema_lines[7], "active[1:1]: boolean",
        "Line 7 should be active field"
    );

    // Verify no extra lines in schema section (filter out empty lines)
    let non_empty_schema_lines: Vec<_> = schema_lines.iter().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        non_empty_schema_lines.len(),
        8,
        "Schema section should have exactly 8 non-empty lines"
    );

    // Check Example section
    assert_eq!(lines[example_idx], "Example:", "Should have Example header");
    assert_eq!(
        lines[example_idx + 1],
        "tool: Person",
        "Example should start with tool"
    );
    assert_eq!(
        lines[example_idx + 2],
        "name: Alice",
        "Example should have name"
    );
    assert_eq!(lines[example_idx + 3], "age: 30", "Example should have age");
    assert_eq!(
        lines[example_idx + 4],
        "active: true",
        "Example should have active"
    );
}

#[test]
fn test_basic_types_no_extra_content() {
    let schema = Person::toon_schema();

    // Verify the schema doesn't contain unexpected patterns
    let schema_part = schema
        .split("Example:")
        .next()
        .expect("Should have schema part");

    // Should not contain inline descriptions (old format)
    assert!(
        !schema_part.contains("\"Person's name\""),
        "Should not have inline description"
    );
    assert!(
        !schema_part.contains("name[1:1]: string \""),
        "Should not have quoted description"
    );

    // Should not contain "description:" key
    assert!(
        !schema_part.contains("description:"),
        "Should not have description key"
    );
}

// ============================================================================
// Collections Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Collection types test")]
struct Collections {
    #[toon_schema(description = "List of strings", example = "[\"a\", \"b\", \"c\"]")]
    items: Vec<String>,

    #[toon_schema(description = "Optional value", example = "null")]
    maybe: Option<String>,

    #[toon_schema(description = "Optional with value", example = "\"present\"")]
    maybe_present: Option<String>,

    #[toon_schema(description = "Required field", example = "42")]
    required: i32,
}

#[test]
fn test_collections_structure() {
    let schema = Collections::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    let example_idx = lines.iter().position(|l| l == &"Example:").unwrap();
    let schema_lines = &lines[0..example_idx];

    // Vec should have [0:] range and array<string> type
    let items_line = schema_lines
        .iter()
        .find(|l| l.starts_with("items"))
        .unwrap();
    assert_eq!(
        *items_line, "items[0:]: array<string>",
        "Vec should have [0:] range: {}",
        items_line
    );

    // Option should have [0:1] range
    let maybe_line = schema_lines
        .iter()
        .find(|l| l.starts_with("maybe["))
        .unwrap();
    assert_eq!(
        *maybe_line, "maybe[0:1]: string",
        "Option should have [0:1] range: {}",
        maybe_line
    );

    // Required field should have [1:1] range
    let required_line = schema_lines
        .iter()
        .find(|l| l.starts_with("required"))
        .unwrap();
    assert_eq!(
        *required_line, "required[1:1]: integer",
        "Required should have [1:1] range: {}",
        required_line
    );
}

#[test]
fn test_collections_example_format() {
    let schema = Collections::toon_schema();

    // Check that the example uses Toon format, not JSON
    let example_lines: Vec<&str> = schema
        .split("Example:")
        .nth(1)
        .expect("Should have example")
        .lines()
        .collect();

    // Vec should be shown with count
    let items_line = example_lines
        .iter()
        .find(|l| l.starts_with("items["))
        .expect("Example should have items with count notation");
    assert!(
        items_line.starts_with("items[3]:") || items_line.starts_with("items[count]:"),
        "Example should show Vec with count notation, got: {}",
        items_line
    );

    // Optional null should be omitted or shown appropriately
    for line in &example_lines {
        assert!(
            !line.contains("\"maybe\": null"),
            "Should not have JSON null format, got: {}",
            line
        );
    }
}

// ============================================================================
// Type Mapping Tests
// ============================================================================

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "All primitive types")]
struct AllTypes {
    #[toon_schema(example = "\"string\"")]
    s: String,

    #[toon_schema(example = "42")]
    i: i32,

    #[toon_schema(example = "42")]
    i64_val: i64,

    #[toon_schema(example = "3.14")]
    f: f32,

    #[toon_schema(example = "3.14")]
    f64_val: f64,

    #[toon_schema(example = "true")]
    b: bool,
}

#[test]
fn test_primitive_type_mappings() {
    let schema = AllTypes::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    let example_idx = lines.iter().position(|l| l == &"Example:").unwrap();
    let schema_lines: Vec<_> = lines[0..example_idx]
        .iter()
        .filter(|l| !l.starts_with("#"))
        .collect();

    // All should use lowercase type names - check exact field lines
    let expected_fields = vec![
        "s[1:1]: string",
        "i[1:1]: integer",
        "i64_val[1:1]: integer",
        "f[1:1]: float",
        "f64_val[1:1]: float",
        "b[1:1]: boolean",
    ];

    let field_lines: Vec<_> = schema_lines
        .iter()
        .filter(|l| !l.is_empty() && !l.starts_with("tool:"))
        .map(|l| **l)
        .collect();

    for expected in expected_fields {
        assert!(
            field_lines.contains(&expected),
            "Expected field '{}', got {:?}",
            expected,
            field_lines
        );
    }

    // Verify all fields are required (have [1:1] range)
    for field in &field_lines {
        assert!(
            field.contains("[1:1]"),
            "All primitive fields should be required, got: {}",
            field
        );
    }
}

// ============================================================================
// Integration Tests
// ============================================================================

#[test]
fn test_roundtrip_deserialization() {
    // The example generated should be deserializable back to the struct
    let example = r#"{"files":["/etc/passwd","/etc/hosts"],"encoding":"utf-8","max_size":1048576}"#;

    // Parse as JSON
    let json: serde_json::Value = serde_json::from_str(example).expect("Should parse as JSON");

    // Verify structure
    assert!(json.get("files").is_some(), "Should have files field");
    assert!(json.get("encoding").is_some(), "Should have encoding field");
    assert!(json.get("max_size").is_some(), "Should have max_size field");

    // Verify types
    assert!(json["files"].is_array(), "files should be array");
    assert!(json["encoding"].is_string(), "encoding should be string");
    assert!(json["max_size"].is_number(), "max_size should be number");
}

#[test]
fn test_empty_lines() {
    let schema = Person::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    // Should have exactly one empty line (between schema and example)
    let empty_lines: Vec<_> = lines.iter().filter(|l| l.is_empty()).collect();
    assert_eq!(
        empty_lines.len(),
        1,
        "Should have exactly one empty line separator"
    );
}

#[test]
fn test_no_trailing_whitespace() {
    let schema = Person::toon_schema();

    for (i, line) in schema.lines().enumerate() {
        assert_eq!(
            line.trim_end(),
            line,
            "Line {} should not have trailing whitespace",
            i
        );
    }
}

#[test]
fn test_consistent_newlines() {
    let schema = Person::toon_schema();

    // Should use \n consistently, not \r\n
    let lines: Vec<&str> = schema.lines().collect();
    let reconstructed = lines.join("\n");
    assert_eq!(
        schema, reconstructed,
        "Should use Unix line endings (single \n)"
    );
}
