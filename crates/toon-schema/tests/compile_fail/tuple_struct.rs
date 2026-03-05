use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Tuple struct test")]
struct TupleStruct(
    #[toon_schema(example = "\"value\"")] String,
);

fn main() {}
