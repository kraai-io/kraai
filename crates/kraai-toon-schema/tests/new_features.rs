use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "collections",
    description: "Collection range coverage",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Collections {
            #[toon_schema(description = "At least one path", min = 1)]
            paths: Vec<String>,

            #[toon_schema(description = "Up to three values", max = 3)]
            values: Vec<i32>,

            #[toon_schema(description = "Exactly two entries")]
            pair: [i32; 2],

            #[toon_schema(description = "Optional note")]
            note: Option<String>,
        }
    },
    root: Collections,
    examples: [
        { paths: ["a"], values: [1, 2], pair: [7, 9] }
    ]
}

#[test]
fn renders_vec_ranges_and_fixed_arrays() {
    let schema = Collections::toon_schema();
    assert!(schema.contains("paths[1:]: array<string>"));
    assert!(schema.contains("values[0:3]: array<integer>"));
    assert!(schema.contains("pair[2:2]: array<integer>"));
    assert!(schema.contains("note[0:1]: string"));
}
