use super::*;
use serde_json::json;

#[test]
fn disabled_tools_can_retain_a_declaration_for_tool_history() {
    let tool = || FunctionTool::from(kraai_provider_core::ScriptToolDefinition::nushell());
    for (tools, tool_choice, expected) in [
        (None, None, None),
        (Some(vec![tool()]), Some("none"), Some(json!([tool()]))),
    ] {
        let request = ChatCompletionRequest {
            model: "fixture".into(),
            messages: Vec::new(),
            tools,
            tool_choice,
            max_completion_tokens: None,
            parallel_tool_calls: None,
            stream: true,
            stream_options: None,
            prompt_cache_key: None,
        };
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded.get("tools"), expected.as_ref());
        assert_eq!(
            encoded.get("tool_choice"),
            tool_choice.map(|choice| json!(choice)).as_ref()
        );
    }
}
