use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"field":null}"#)]
struct NullForNonOptional {
    #[toon_schema(description = "Field")]
    field: String,
}

fn main() {}
