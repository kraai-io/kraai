use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Case-sensitive keyword tests",
    example = r#"{"lower_true":"true","lower_false":"false","lower_null":"null","upper_true":"TRUE","mixed_false":"False","upper_null":"NULL"}"#
)]
struct CaseSensitiveKeywords {
    #[toon_schema(description = "Lower true")]
    lower_true: String,
    #[toon_schema(description = "Lower false")]
    lower_false: String,
    #[toon_schema(description = "Lower null")]
    lower_null: String,
    #[toon_schema(description = "Upper true")]
    upper_true: String,
    #[toon_schema(description = "Mixed false")]
    mixed_false: String,
    #[toon_schema(description = "Upper null")]
    upper_null: String,
}

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Structural strings",
    example = r#"{"just_dash":"-","dash_test":"-test","dash_in_middle":"hello-world","with_colon":"key:value","with_quote":"say \"hello\"","safe":"hello_world"}"#
)]
struct StructuralStrings {
    #[toon_schema(description = "Just dash")]
    just_dash: String,
    #[toon_schema(description = "Dash test")]
    dash_test: String,
    #[toon_schema(description = "Dash middle")]
    dash_in_middle: String,
    #[toon_schema(description = "Colon")]
    with_colon: String,
    #[toon_schema(description = "Quote")]
    with_quote: String,
    #[toon_schema(description = "Safe")]
    safe: String,
}

#[test]
fn test_case_sensitive_keywords() {
    let schema = CaseSensitiveKeywords::toon_schema();
    assert!(schema.contains("lower_true: \"true\""));
    assert!(schema.contains("lower_false: \"false\""));
    assert!(schema.contains("lower_null: \"null\""));
    assert!(schema.contains("upper_true: TRUE"));
    assert!(schema.contains("mixed_false: False"));
    assert!(schema.contains("upper_null: NULL"));
}

#[test]
fn test_structural_strings_are_quoted_only_when_needed() {
    let schema = StructuralStrings::toon_schema();
    assert!(schema.contains("just_dash: \"-\""));
    assert!(schema.contains("dash_test: \"-test\""));
    assert!(schema.contains("dash_in_middle: \"hello-world\""));
    assert!(schema.contains("with_colon: \"key:value\""));
    assert!(schema.contains("with_quote: \"say \\\"hello\\\"\""));
    assert!(schema.contains("safe: hello_world"));
}
