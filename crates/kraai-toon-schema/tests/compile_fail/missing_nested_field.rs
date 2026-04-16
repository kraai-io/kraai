use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "missing_nested_field",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Child {
            value: String,
        }

        #[derive(serde::Deserialize, serde::Serialize)]
        struct Root {
            child: Child,
        }
    },
    root: Root,
    examples: [
        { child: {} }
    ]
}

fn main() {}
