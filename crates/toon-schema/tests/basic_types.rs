use toon_schema::toon_tool;

toon_tool! {
    name: "person",
    description: "Basic tool schema test",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Person {
            #[toon_schema(description = "Person's name")]
            name: String,
            #[toon_schema(description = "Person's age")]
            age: i32,
            #[toon_schema(description = "Is person active")]
            active: bool,
        }
    },
    root: Person,
    examples: [
        { name: "Alice", age: 30, active: true },
        { name: "Bob", age: 25, active: false },
    ]
}

#[test]
fn generates_tool_name_and_schema() {
    assert_eq!(Person::tool_name(), "person");

    let schema = Person::toon_schema();
    assert!(schema.contains("# Basic tool schema test"));
    assert!(schema.contains("tool: person"));
    assert!(schema.contains("name[1:1]: string"));
    assert!(schema.contains("age[1:1]: integer"));
    assert!(schema.contains("active[1:1]: boolean"));
    assert_eq!(schema.matches("<tool_call>").count(), 2);
}
