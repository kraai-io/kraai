use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "numeric_example_overflow",
    types: {
        struct Input {
            value: u128,
        }
    },
    root: Input,
    examples: [
        { value: 18446744073709551616 }
    ]
}

fn main() {}
