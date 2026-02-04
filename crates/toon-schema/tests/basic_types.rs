use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

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
fn test_basic_types() {
    let schema = Person::toon_schema();

    // Check that schema contains expected parts
    assert!(
        schema.contains("tool: Person"),
        "Schema should contain tool name"
    );
    assert!(
        schema.contains("# A simple person struct"),
        "Schema should contain description as comment"
    );
    assert!(
        schema.contains("# Person's name"),
        "Schema should contain name description"
    );
    assert!(
        schema.contains("name[1:1]: string"),
        "Schema should contain name field with type"
    );
    assert!(
        schema.contains("# Person's age"),
        "Schema should contain age description"
    );
    assert!(
        schema.contains("age[1:1]: integer"),
        "Schema should contain age field with type"
    );
    assert!(
        schema.contains("# Is person active"),
        "Schema should contain active description"
    );
    assert!(
        schema.contains("active[1:1]: boolean"),
        "Schema should contain active field with type"
    );

    // Check example section
    assert!(
        schema.contains("Example:"),
        "Schema should contain example section"
    );
    assert!(
        schema.contains("tool: Person"),
        "Example should contain tool name"
    );
    assert!(
        schema.contains("name: Alice"),
        "Example should contain name"
    );
    assert!(schema.contains("age: 30"), "Example should contain age");
    assert!(
        schema.contains("active: true"),
        "Example should contain active"
    );
}

#[test]
fn test_schema_structure() {
    let schema = Person::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    // First line should be description comment
    assert_eq!(lines[0], "# A simple person struct");

    // Second line should be tool name
    assert_eq!(lines[1], "tool: Person");

    // Should have field definitions (look for type patterns)
    let field_lines: Vec<_> = lines.iter().filter(|l| l.contains("[1:1]:")).collect();
    assert_eq!(field_lines.len(), 3, "Should have 3 required fields");
}
