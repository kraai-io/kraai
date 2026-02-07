use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

// Define a type alias to represent an enum type
#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Test enum support")]
struct EnumExample {
    #[toon_schema(
        description = "Status of the request",
        example = "\"pending\"",
        variants = "pending|active|completed|failed"
    )]
    status: Status,
}

// Type alias for clarity
type Status = String;

#[test]
fn test_enum_schema_generation() {
    let schema = EnumExample::toon_schema();

    // Check that the schema includes the enum type
    assert!(
        schema.contains("status[1:1]: enum<pending|active|completed|failed>"),
        "Schema should contain enum type with variants"
    );

    // Check that the description is included
    assert!(
        schema.contains("# Status of the request"),
        "Schema should contain field description"
    );
}

#[test]
fn test_enum_example_in_output() {
    let schema = EnumExample::toon_schema();

    // Check that the example section exists
    assert!(
        schema.contains("Example:"),
        "Schema should contain example section"
    );
    assert!(
        schema.contains("status: pending"),
        "Example should show enum value"
    );
}

// Test with optional enum
#[derive(ToonSchema, Serialize, Deserialize)]
struct OptionalEnumExample {
    #[toon_schema(
        description = "Optional priority",
        example = "\"high\"",
        variants = "low|medium|high|critical"
    )]
    priority: Option<Priority>,
}

type Priority = String;

#[test]
fn test_optional_enum() {
    let schema = OptionalEnumExample::toon_schema();

    // Should show [0:1] for optional field
    assert!(
        schema.contains("priority[0:1]: enum<low|medium|high|critical>"),
        "Optional enum should have [0:1] range"
    );
}
