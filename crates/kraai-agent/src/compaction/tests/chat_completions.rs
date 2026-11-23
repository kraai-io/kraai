use super::*;

#[tokio::test]
async fn chat_completions_compaction_replays_tools_and_resumes_with_native_calls() {
    use futures::StreamExt;
    use kraai_provider_core::{DynamicConfig, DynamicValue, ProviderFactory};
    use kraai_provider_openai_chat_completions::OpenAiChatCompletionsFactory;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut listener = kraai_test_support::http::Listener::bind("/chat/completions")
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in [
            json!({"choices":[{"index":0,"delta":{"content":"Preserve the filesystem; continue inspecting."},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":10}}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"next-call","type":"function","function":{"name":"kraai_nushell","arguments":"{\"input\":\"# timeout=1sec\\nls\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":20,"completion_tokens":5}}),
        ] {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = Vec::new();
            let body_start = loop {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await?;
                ensure!(read != 0, "request ended before headers");
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..body_start])?;
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim())
                })
                .ok_or_else(|| eyre!("missing content-length"))?
                .parse()?;
            while bytes.len() < body_start + length {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await?;
                ensure!(read != 0, "request ended before body");
                bytes.extend_from_slice(&chunk[..read]);
            }
            requests.push(serde_json::from_slice::<Value>(
                &bytes[body_start..body_start + length],
            )?);
            let body = format!("data: {response}\n\ndata: [DONE]\n\n");
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
        }
        Ok::<_, color_eyre::Report>(requests)
    });
    let mut provider = OpenAiChatCompletionsFactory::create(
        ProviderId::new("test"),
        DynamicConfig::from([
            (
                "base_url".into(),
                DynamicValue::from(format!("http://{address}")),
            ),
            ("api_key".into(), DynamicValue::from("fixture")),
        ]),
    )
    .unwrap();
    provider
        .register_model(ModelConfig {
            id: ModelId::new("model"),
            provider_id: ProviderId::new("test"),
            options: Default::default(),
            remove_options: Default::default(),
            config: DynamicConfig::from([("supports_images".into(), DynamicValue::Bool(true))]),
        })
        .await
        .unwrap();
    let (mut context, mut providers, _, root) = fixture(false, vec![], false);
    let image = kraai_execution_types::ImageAttachment {
        id: "a".repeat(64),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
        byte_length: 3,
    };
    for message in &mut context.original.messages {
        if let ConversationItem::ScriptResult { output, .. } = message {
            output.0.push(kraai_types::ContentPart::Image {
                image: image.clone(),
            });
        }
    }
    context = context.with_image_resolver(Arc::new(TestImageResolver));
    providers.register_provider(ProviderId::new("test"), provider);
    let outcome = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await
        .unwrap();
    assert!(outcome.request.script_tool.is_some());
    assert!(outcome.request.messages.iter().any(|message| {
        message
            .display_text()
            .contains("Preserve the filesystem; continue inspecting.")
    }));
    assert!(
        !outcome
            .request
            .messages
            .iter()
            .any(|message| matches!(message, ConversationItem::ScriptResult { .. }))
    );
    let mut stream = providers
        .generate_reply_stream(
            ProviderId::new("test"),
            &ModelId::new("model"),
            outcome.request,
            ProviderRequestContext::default(),
        )
        .await
        .unwrap();
    let mut calls = 0;
    while let Some(event) = stream.next().await {
        if let ProviderStreamEvent::ScriptCall(ScriptCall { call_id, input, .. }) = event.unwrap() {
            assert!(call_id.as_str() == "next-call");
            assert!(input == "# timeout=1sec\nls");
            calls += 1;
        }
    }
    assert!(calls == 1);
    let requests = kraai_test_support::hang_guard(server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first = requests.first().expect("missing summary request");
    assert!(first.pointer("/tools/0/function/name") == Some(&json!("kraai_nushell")));
    assert!(first.get("tool_choice") == Some(&json!("auto")));
    assert!(first.get("parallel_tool_calls") == Some(&json!(false)));
    assert!(first.to_string().contains("pinned files"));
    let messages = first
        .get("messages")
        .and_then(Value::as_array)
        .expect("missing messages");
    assert!(
        messages
            .iter()
            .any(|message| message.get("tool_calls").is_some())
    );
    assert!(
        messages
            .iter()
            .any(|message| message.get("role") == Some(&json!("tool"))
                && message.get("tool_call_id") == Some(&json!("call")))
    );
    assert!(
        messages
            .iter()
            .any(|message| message.get("role") == Some(&json!("user"))
                && message.to_string().contains("data:image/png;base64,AQID"))
    );
    let resumed = requests.get(1).expect("missing resumed request");
    assert!(resumed.get("tool_choice") == Some(&json!("auto")));
    assert!(first.get("tools") == resumed.get("tools"));
    assert!(resumed.get("parallel_tool_calls") == Some(&json!(false)));
    assert!(
        resumed
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| tools.len() == 1)
    );
    tokio::fs::remove_dir_all(root).await.unwrap();
}
