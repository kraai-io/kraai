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
        schema.contains("Person:"),
        "Schema should contain struct name"
    );
    assert!(
        schema.contains("description: \"A simple person struct\""),
        "Schema should contain description"
    );
    assert!(
        schema.contains("name[1:1]: string \"Person's name\""),
        "Schema should contain name field"
    );
    assert!(
        schema.contains("age[1:1]: integer \"Person's age\""),
        "Schema should contain age field"
    );
    assert!(
        schema.contains("active[1:1]: boolean \"Is person active\""),
        "Schema should contain active field"
    );

    // Check example section
    assert!(
        schema.contains("Example:"),
        "Schema should contain example section"
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

    // First line should be struct name
    assert_eq!(lines[0], "Person:");

    // Second line should be description
    assert!(lines[1].contains("description:"));

    // Should have exactly 3 field lines (name, age, active)
    let field_lines: Vec<_> = lines.iter().filter(|l| l.contains("[1:1]:")).collect();
    assert_eq!(field_lines.len(), 3, "Should have 3 required fields");
}
