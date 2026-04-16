use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "tuple_struct",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Root(String);
    },
    root: Root,
    examples: []
}

fn main() {}
