use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "invalid_range_order",
    types: {
        struct Input {
            #[toon_schema(min = 2, max = 1)]
            values: Vec<String>,
        }
    },
    root: Input,
    examples: []
}

fn main() {}
