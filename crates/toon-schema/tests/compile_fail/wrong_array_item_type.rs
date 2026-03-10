use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"items":["ok",1]}"#)]
struct WrongArrayItemType {
    #[toon_schema(description = "Items")]
    items: Vec<String>,
}

fn main() {}
