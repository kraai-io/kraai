use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"items":["a","b","c"]}"#)]
struct ArrayAboveMax {
    #[toon_schema(description = "Items", max = 2)]
    items: Vec<String>,
}

fn main() {}
