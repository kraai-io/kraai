use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "person",
    description: "Simple example schema",
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
        { name: "Alice", age: 30, active: true }
    ]
}

fn main() {
    println!("{}", Person::toon_schema());
}
