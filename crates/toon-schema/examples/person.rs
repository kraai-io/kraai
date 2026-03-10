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

fn main() {
    println!("{}", Person::toon_schema());
}
