use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = "not valid json")]
struct MalformedJson {
    #[toon_schema(description = "A field")]
    field: String,
}

fn main() {}
