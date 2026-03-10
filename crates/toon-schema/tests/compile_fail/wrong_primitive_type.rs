use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"count":"not a number"}"#)]
struct WrongPrimitiveType {
    #[toon_schema(description = "Count")]
    count: i32,
}

fn main() {}
