use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Custom ranges test",
    example = r#"{"at_least_one":["a"],"bounded":["a","b"],"max_only":[],"standard_vec":[]}"#,
    example = r#"{"at_least_one":["z"],"bounded":["x"],"max_only":["y"],"standard_vec":["q","w"]}"#
)]
struct CustomRanges {
    #[toon_schema(description = "At least one", min = 1)]
    at_least_one: Vec<String>,

    #[toon_schema(description = "Bounded", min = 1, max = 5)]
    bounded: Vec<String>,

    #[toon_schema(description = "Max only", max = 3)]
    max_only: Vec<String>,

    #[toon_schema(description = "Standard vec")]
    standard_vec: Vec<String>,
}

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Default values test",
    example = r#"{"required":42}"#,
    example = r#"{"greeting":"hello","enabled":true,"required":7}"#
)]
struct WithDefaults {
    #[toon_schema(description = "With default")]
    #[serde(default = "default_greeting")]
    greeting: String,

    #[toon_schema(description = "With default bool")]
    #[serde(default)]
    enabled: bool,

    #[toon_schema(description = "No default")]
    required: i32,
}

fn default_greeting() -> String {
    "world".to_string()
}

#[test]
fn test_custom_ranges() {
    let schema = CustomRanges::toon_schema();
    assert!(schema.contains("at_least_one[1:]: array<string>"));
    assert!(schema.contains("bounded[1:5]: array<string>"));
    assert!(schema.contains("max_only[0:3]: array<string>"));
    assert!(schema.contains("standard_vec[0:]: array<string>"));
    assert!(schema.contains("bounded[2]: a,b"));
    assert!(schema.contains("standard_vec[2]: q,w"));
}

#[test]
fn test_default_values() {
    let schema = WithDefaults::toon_schema();
    assert!(schema.contains("greeting[1:1]: string # default: default_greeting"));
    assert!(schema.contains("enabled[1:1]: boolean # default: default"));
    assert!(schema.contains("required[1:1]: integer"));
    assert!(!schema.contains("\nrequired[1:1]: integer # default:"));
    assert!(schema.contains("<tool_call>\ntool: WithDefaults\nrequired: 42\n</tool_call>"));
}

#[test]
fn test_compile_time_schema() {
    let schema: &'static str = CustomRanges::toon_schema();
    let schema2 = CustomRanges::toon_schema();
    assert!(!schema.is_empty());
    assert_eq!(schema, schema2);
}
