use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"items":[]}"#)]
struct ArrayBelowMin {
    #[toon_schema(description = "Items", min = 1)]
    items: Vec<String>,
}

fn main() {}
