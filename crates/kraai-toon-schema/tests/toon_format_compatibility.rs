use kraai_toon_schema::toon_tool;
use serde_json::json;

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
    name: "edit_like",
    description: "Array of objects should match toon-format rendering",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct Edit {
            old_text: String,
            new_text: String,
        }

        #[derive(serde::Deserialize, serde::Serialize)]
        struct EditLike {
            path: String,
            create: bool,
            edits: Vec<Edit>,
        }
    },
    root: EditLike,
    examples: [
        {
            path: "src/lib.rs",
            create: false,
            edits: [
                { old_text: "old", new_text: "new" }
            ]
        }
    ]
}

#[test]
fn array_of_objects_examples_match_toon_format_exactly() {
    let expected = toon_format::encode_default(&json!({
        "path": "src/lib.rs",
        "create": false,
        "edits": [
            { "old_text": "old", "new_text": "new" }
        ]
    }))
    .expect("encode");

    let expected_lines: Vec<String> = expected.lines().map(ToOwned::to_owned).collect();
    assert_eq!(
        extract_first_example_lines(EditLike::toon_schema()),
        expected_lines
    );
}
