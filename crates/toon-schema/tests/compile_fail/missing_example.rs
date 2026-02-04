use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
struct MissingExample {
    #[toon_schema(description = "This field is missing an example")]
    // ERROR: Missing example attribute
    field: String,
}

fn main() {}
