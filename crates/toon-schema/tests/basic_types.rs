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
    let lines: Vec<&str> = schema.lines().collect();

    // Verify exact schema structure line by line
    // # A simple person struct
    // tool: Person
    // # Person's name
    // name[1:1]: string
    // # Person's age
    // age[1:1]: integer
    // # Is person active
    // active[1:1]: boolean
    // <blank>
    // Example:
    // tool: Person
    // name: Alice
    // age: 30
    // active: true
    assert_eq!(
        lines[0], "# A simple person struct",
        "First line should be description comment"
    );
    assert_eq!(lines[1], "tool: Person", "Second line should be tool name");
    assert_eq!(
        lines[2], "# Person's name",
        "Third line should be name description"
    );
    assert_eq!(
        lines[3], "name[1:1]: string",
        "Fourth line should be name field with type"
    );
    assert_eq!(
        lines[4], "# Person's age",
        "Fifth line should be age description"
    );
    assert_eq!(
        lines[5], "age[1:1]: integer",
        "Sixth line should be age field with type"
    );
    assert_eq!(
        lines[6], "# Is person active",
        "Seventh line should be active description"
    );
    assert_eq!(
        lines[7], "active[1:1]: boolean",
        "Eighth line should be active field with type"
    );

    // Check example section
    assert_eq!(lines[8], "", "Ninth line should be empty separator");
    assert_eq!(lines[9], "Example:", "Tenth line should be example header");
    assert_eq!(
        lines[10], "tool: Person",
        "Eleventh line should be example tool name"
    );
    assert_eq!(
        lines[11], "name: Alice",
        "Twelfth line should be example name"
    );
    assert_eq!(
        lines[12], "age: 30",
        "Thirteenth line should be example age"
    );
    assert_eq!(
        lines[13], "active: true",
        "Fourteenth line should be example active"
    );
}

#[test]
fn test_schema_structure() {
    let schema = Person::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    // First line should be description comment
    assert_eq!(
        lines[0], "# A simple person struct",
        "First line should be description"
    );

    // Second line should be tool name
    assert_eq!(lines[1], "tool: Person", "Second line should be tool name");

    // Should have exactly 3 required fields (field lines with [1:1] range)
    let field_lines: Vec<String> = lines
        .iter()
        .filter(|l| l.contains("[1:1]:"))
        .map(|l| l.to_string())
        .collect();
    assert_eq!(
        field_lines,
        vec![
            "name[1:1]: string",
            "age[1:1]: integer",
            "active[1:1]: boolean"
        ],
        "Should have 3 required fields with exact format"
    );
}
