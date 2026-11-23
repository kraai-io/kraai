use super::*;
use serde_json::json;

fn text(text: &str) -> RequestContent {
    RequestContent::Text(text.into())
}

#[test]
fn marks_the_system_prompt_and_the_two_latest_messages_with_text() {
    let mut messages = vec![
        RequestMessage::System {
            content: text("system"),
        },
        RequestMessage::User {
            content: text("first"),
        },
        RequestMessage::User {
            content: text("second"),
        },
        RequestMessage::Assistant {
            reasoning: Default::default(),
            content: Some("reply".into()),
            tool_calls: Vec::new(),
        },
        RequestMessage::Tool {
            tool_call_id: kraai_types::ToolCallId::new("call"),
            content: text("result"),
        },
        RequestMessage::Tool {
            tool_call_id: kraai_types::ToolCallId::new("empty"),
            content: text(""),
        },
    ];
    mark_breakpoints(&mut messages);
    let marked = |text: &str| json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]);
    let body = serde_json::to_value(&messages).unwrap_or_default();
    assert_eq!(body[0]["content"], marked("system"));
    assert_eq!(body[1]["content"], json!("first"));
    assert_eq!(body[2]["content"], marked("second"));
    assert_eq!(body[3]["content"], json!("reply"));
    assert_eq!(body[4]["content"], marked("result"));
    assert_eq!(body[5]["content"], json!(""));
}

#[test]
fn marks_the_last_text_part_of_multipart_content() {
    let mut messages = vec![RequestMessage::User {
        content: RequestContent::Parts(vec![
            RequestContentPart::Text {
                text: "look".into(),
                cache_control: None,
            },
            RequestContentPart::ImageUrl {
                image_url: crate::wire::ImageUrl {
                    url: "data:image/png;base64,AA==".into(),
                },
            },
        ]),
    }];
    mark_breakpoints(&mut messages);
    let body = serde_json::to_value(&messages).unwrap_or_default();
    assert_eq!(
        body[0]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(body[0]["content"][1].get("cache_control").is_none());
}

#[test]
fn defaults_follow_the_endpoint_and_config_overrides_them() {
    let empty = DynamicConfig::new();
    assert_eq!(
        CacheHints::resolve(
            &empty,
            ModelOptionsProtocol::OpenRouterChatCompletions,
            "https://openrouter.ai/api/v1"
        ),
        CacheHints {
            breakpoints: true,
            prompt_cache_key: false
        }
    );
    assert_eq!(
        CacheHints::resolve(
            &empty,
            ModelOptionsProtocol::OpenAiChatCompletions,
            "https://api.openai.com/v1"
        ),
        CacheHints {
            breakpoints: false,
            prompt_cache_key: true
        }
    );
    let enabled = DynamicConfig::from([
        ("cache_breakpoints".into(), DynamicValue::Bool(true)),
        ("prompt_cache_key".into(), DynamicValue::Bool(true)),
    ]);
    assert_eq!(
        CacheHints::resolve(
            &enabled,
            ModelOptionsProtocol::OpenAiChatCompletions,
            "http://localhost:8080/v1"
        ),
        CacheHints {
            breakpoints: true,
            prompt_cache_key: true
        }
    );
    assert_eq!(
        CacheHints::resolve(
            &empty,
            ModelOptionsProtocol::OpenAiChatCompletions,
            "http://localhost:8080/v1"
        ),
        CacheHints::default()
    );
}
