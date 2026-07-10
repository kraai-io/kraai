use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "invalid_range_string",
    types: {
        struct Input {
            #[toon_schema(min = "one")]
            values: Vec<String>,
        }
    },
    root: Input,
    examples: []
}

fn main() {}
