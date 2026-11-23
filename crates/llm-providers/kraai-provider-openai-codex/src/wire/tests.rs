use super::*;
use serde_json::json;

fn script_tool() -> ResponsesCustomTool {
    ResponsesCustomTool {
        kind: "custom",
        name: String::from("kraai_nushell"),
        description: String::from("Execute Nushell"),
    }
}

#[test]
fn responses_request_registers_exactly_one_raw_custom_tool() {
    let request = ResponsesRequest {
        service_tier: None,
        model: "gpt-5.6-sol".to_string(),
        instructions: "instructions".to_string(),
        input: Vec::<ResponsesRequestItem>::new(),
        reasoning: Some(ResponsesReasoning {
            effort: "high".to_string(),
            context: "all_turns",
        }),
        tools: vec![script_tool()],
        tool_choice: Some("auto"),
        parallel_tool_calls: Some(false),
        max_output_tokens: None,
        stream: true,
        store: false,
        include: vec!["reasoning.encrypted_content"],
        prompt_cache_key: None,
    };

    let serialized = serde_json::to_value(request).expect("serialized request");

    assert_eq!(
        serialized,
        json!({
            "model": "gpt-5.6-sol", "instructions": "instructions", "input": [],
            "reasoning": {"effort": "high", "context": "all_turns"},
            "tools": [{"type": "custom", "name": "kraai_nushell", "description": "Execute Nushell"}],
            "tool_choice": "auto", "parallel_tool_calls": false, "stream": true, "store": false,
            "include": ["reasoning.encrypted_content"],
        })
    );
}
