use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
struct ReservedTool {
    // ERROR: field name 'tool' is reserved
    #[toon_schema(example = "\"value\"")]
    tool: String,
}

fn main() {}
