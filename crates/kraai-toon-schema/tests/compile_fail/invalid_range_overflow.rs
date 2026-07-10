use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "invalid_range_overflow",
    types: {
        struct Input {
            #[toon_schema(max = 4294967296)]
            values: Vec<String>,
        }
    },
    root: Input,
    examples: []
}

fn main() {}
