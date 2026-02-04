use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
struct CustomRangeOnNonVec {
    // ERROR: custom ranges can only be applied to Vec<T> fields
    #[toon_schema(example = "\"value\"", min = 1)]
    name: String,
}

fn main() {}
