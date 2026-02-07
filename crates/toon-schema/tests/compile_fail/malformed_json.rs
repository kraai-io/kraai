use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
struct MalformedJson {
    // ERROR: Invalid JSON in example
    #[toon_schema(description = "A field", example = "not valid json")]
    field: String,
}

fn main() {}
