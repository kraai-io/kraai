use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{}"#)]
struct MissingRequiredFieldInExample {
    #[toon_schema(description = "Field")]
    field: String,
}

fn main() {}
