use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
struct UnknownType {
    // ERROR: Unknown type without enum variants
    #[toon_schema(description = "A custom type", example = "{}")]
    custom_field: SomeCustomType,
}

fn main() {}
