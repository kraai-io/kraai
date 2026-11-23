use super::*;

#[test]
fn script_calls_serialize_as_flat_tagged_objects() {
    let call = AssistantItem::ScriptCall(ScriptCall {
        call_id: ToolCallId::new("call"),
        name: "kraai_nushell".into(),
        input: "ls".into(),
        arguments_error: None,
        rewrite: None,
    });
    assert_eq!(
        serde_json::to_value(&call).unwrap(),
        serde_json::json!({
            "type": "script_call",
            "call_id": "call",
            "name": "kraai_nushell",
            "input": "ls",
            "arguments_error": null,
            "rewrite": null,
        })
    );
    let without_error = serde_json::json!({
        "type": "script_call",
        "call_id": "call",
        "name": "kraai_nushell",
        "input": "ls",
    });
    assert_eq!(
        serde_json::from_value::<AssistantItem>(without_error).unwrap(),
        call
    );
}

#[test]
fn ordered_content_roundtrips_without_embedding_image_bytes() {
    let image = ImageAttachment {
        id: "a".repeat(64),
        mime_type: "image/png".into(),
        width: 2,
        height: 3,
        byte_length: 100,
    };
    let content = MessageContent(vec![
        ContentPart::Text {
            text: "before".into(),
        },
        ContentPart::Image {
            image: image.clone(),
        },
        ContentPart::Text {
            text: "after".into(),
        },
    ]);
    let encoded = serde_json::to_string(&content).unwrap();
    assert_eq!(
        serde_json::from_str::<MessageContent>(&encoded).unwrap(),
        content
    );
    assert!(content.display_text().starts_with("before\n[Image "));
    assert!(content.display_text().ends_with("\nafter"));
    assert_eq!(content.images().collect::<Vec<_>>(), vec![&image]);
    assert!(!content.without_images().has_images());
    assert!(content.without_images().display_text().contains(&image.id));
}
