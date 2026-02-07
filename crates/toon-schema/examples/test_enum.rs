use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Test enum support")]
struct EnumExample {
    #[toon_schema(
        description = "Status of the request",
        example = "\"pending\"",
        variants = "pending|active|completed|failed"
    )]
    status: Status,
}

type Status = String;

fn main() {
    println!("Schema output:");
    println!("{}", EnumExample::toon_schema());
}
