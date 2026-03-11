use serde_json::json;
use toon_schema::toon_tool;

fn extract_first_example_lines(schema: &str) -> Vec<String> {
    let lines: Vec<&str> = schema.lines().collect();
    let tool_call_start = lines
        .iter()
        .position(|line| *line == "<tool_call>")
        .expect("tool call start");
    let tool_call_end = lines
        .iter()
        .position(|line| *line == "</tool_call>")
        .expect("tool call end");

    lines[tool_call_start + 1..tool_call_end]
        .iter()
        .filter(|line| !line.starts_with("tool:"))
        .map(|line| line.to_string())
        .collect()
}

toon_tool! {
    name: "quoted_strings",
    description: "Quoted string coverage",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct QuotedStrings {
            #[toon_schema(description = "Message")]
            message: String,
            #[toon_schema(description = "Tokens")]
            tokens: Vec<String>,
        }
    },
    root: QuotedStrings,
    examples: [
        {
            message: "key: value, [array], {obj} - dash",
            tokens: ["true", "42", "-3.14"]
        }
    ]
}

#[test]
fn example_rendering_matches_toon_format_for_quoted_strings() {
    let expected = toon_format::encode_default(&json!({
        "message": "key: value, [array], {obj} - dash",
        "tokens": ["true", "42", "-3.14"]
    }))
    .expect("encode");

    let expected_lines: Vec<String> = expected.lines().map(ToOwned::to_owned).collect();
    assert_eq!(extract_first_example_lines(QuotedStrings::toon_schema()), expected_lines);
}
