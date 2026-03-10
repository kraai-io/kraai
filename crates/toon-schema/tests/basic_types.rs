use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "A simple person struct",
    example = r#"{"name":"Alice","age":30,"active":true}"#,
    example = r#"{"name":"Bob","age":41,"active":false}"#
)]
struct Person {
    #[toon_schema(description = "Person's name")]
    name: String,

    #[toon_schema(description = "Person's age")]
    age: i32,

    #[toon_schema(description = "Is person active")]
    active: bool,
}

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "A greeting tool",
    name = "say_hello",
    example = r#"{"name":"World"}"#
)]
struct GreetingArgs {
    #[toon_schema(description = "Name to greet")]
    name: String,
}

#[test]
fn test_tool_name() {
    assert_eq!(Person::tool_name(), "Person");
}

#[test]
fn test_custom_tool_name() {
    assert_eq!(GreetingArgs::tool_name(), "say_hello");
}

#[test]
fn test_basic_types_schema_and_examples() {
    let schema = Person::toon_schema();
    let lines: Vec<&str> = schema.lines().collect();

    assert_eq!(lines[0], "# A simple person struct");
    assert_eq!(lines[1], "tool: Person");
    assert_eq!(lines[2], "# Person's name");
    assert_eq!(lines[3], "name[1:1]: string");
    assert_eq!(lines[4], "# Person's age");
    assert_eq!(lines[5], "age[1:1]: integer");
    assert_eq!(lines[6], "# Is person active");
    assert_eq!(lines[7], "active[1:1]: boolean");
    assert_eq!(lines[8], "");
    assert_eq!(lines[9], "Examples:");
    assert_eq!(lines[10], "<tool_call>");
    assert_eq!(lines[11], "tool: Person");
    assert_eq!(lines[12], "name: Alice");
    assert_eq!(lines[13], "age: 30");
    assert_eq!(lines[14], "active: true");
    assert_eq!(lines[15], "</tool_call>");
    assert_eq!(lines[16], "");
    assert_eq!(lines[17], "<tool_call>");
    assert_eq!(lines[18], "tool: Person");
    assert_eq!(lines[19], "name: Bob");
    assert_eq!(lines[20], "age: 41");
    assert_eq!(lines[21], "active: false");
    assert_eq!(lines[22], "</tool_call>");
}

#[test]
fn test_schema_structure() {
    let schema = Person::toon_schema();
    let field_lines: Vec<&str> = schema
        .lines()
        .filter(|line| line.contains("[1:1]:"))
        .collect();

    assert_eq!(
        field_lines,
        vec![
            "name[1:1]: string",
            "age[1:1]: integer",
            "active[1:1]: boolean",
        ]
    );
}
