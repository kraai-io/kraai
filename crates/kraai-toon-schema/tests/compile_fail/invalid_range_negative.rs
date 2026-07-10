use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "invalid_range_negative",
    types: {
        struct Input {
            #[toon_schema(min = -1)]
            values: Vec<String>,
        }
    },
    root: Input,
    examples: []
}

fn main() {}
