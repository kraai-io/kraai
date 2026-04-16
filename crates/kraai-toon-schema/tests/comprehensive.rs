use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "nested_config",
    description: "Nested object and serde behavior coverage",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Limits {
            #[serde(rename = "max_size")]
            #[toon_schema(description = "Maximum size")]
            size: i64,
        }

        #[derive(serde::Deserialize, serde::Serialize)]
        struct NestedConfig {
            #[toon_schema(description = "Nested limits config")]
            limits: Limits,

            #[serde(default)]
            #[toon_schema(description = "Timeout in seconds")]
            timeout: i32,

            #[toon_schema(description = "Labels map")]
            labels: std::collections::BTreeMap<String, String>,
        }
    },
    root: NestedConfig,
    examples: [
        {
            limits: { max_size: 1024 },
            labels: { primary: "yes", secondary: "no" }
        }
    ]
}

#[test]
fn supports_nested_objects_maps_and_defaults() {
    let schema = NestedConfig::toon_schema();
    assert!(schema.contains("limits[1:1]: object"));
    assert!(schema.contains("timeout[1:1]: integer # default: default"));
    assert!(schema.contains("labels[1:1]: map<string, string>"));
    assert!(schema.contains("limits:\n  max_size: 1024"));
}
