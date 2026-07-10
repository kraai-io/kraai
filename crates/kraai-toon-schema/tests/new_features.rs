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

toon_tool! {
    name: "forward_reference",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct ForwardRoot {
            nested: ForwardNested,
        }

        #[derive(serde::Deserialize, serde::Serialize)]
        struct ForwardNested {
            value: String,
        }
    },
    root: ForwardRoot,
    examples: [
        { nested: { value: "ok" } }
    ]
}

#[test]
fn accepts_types_declared_after_their_use() {
    assert_eq!(ForwardRoot::tool_name(), "forward_reference");
}

toon_tool! {
    name: "numeric_examples",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct NumericExamples {
            signed: i64,
            unsigned: u64,
            decimal: f64,
        }
    },
    root: NumericExamples,
    examples: [
        { signed: -1, unsigned: 18446744073709551615, decimal: -1.5 }
    ]
}

#[test]
fn accepts_negative_and_wide_numeric_examples() {
    let schema = NumericExamples::toon_schema();
    assert!(schema.contains("signed: -1"));
    assert!(schema.contains("unsigned: 18446744073709551615"));
    assert!(schema.contains("decimal: -1.5"));
}
