use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"name":"value"}"#)]
struct CustomRangeOnNonVec {
    // ERROR: custom ranges can only be applied to Vec<T> fields
    #[toon_schema(min = 1)]
    name: String,
}

fn main() {}
