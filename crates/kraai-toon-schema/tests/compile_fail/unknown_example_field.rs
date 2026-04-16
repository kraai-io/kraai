use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "unknown_example_field",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Root {
            field: String,
        }
    },
    root: Root,
    examples: [
        { field: "value", extra: true }
    ]
}

fn main() {}
