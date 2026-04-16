use kraai_toon_schema::toon_tool;

struct ExternalType;

toon_tool! {
    name: "external_nested",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Root {
            nested: ExternalType,
        }
    },
    root: Root,
    examples: [
        { nested: null }
    ]
}

fn main() {}
