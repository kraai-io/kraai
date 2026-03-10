use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"field":"value"}"#)]
struct FieldLevelExample {
    #[toon_schema(example = "\"value\"")]
    field: String,
}

fn main() {}
