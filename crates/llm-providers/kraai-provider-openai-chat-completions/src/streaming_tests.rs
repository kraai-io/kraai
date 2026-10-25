#![expect(
    clippy::indexing_slicing,
    clippy::panic_in_result_fn,
    reason = "tests assert wire fixtures"
)]
use super::*;
use futures::{StreamExt, stream};
use serde_json::{Value, json};

fn data(value: Value) -> Result<SseEvent> {
    Ok(SseEvent::Data(value.to_string()))
}

fn call(arguments: &str) -> Value {
    json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"kraai_nushell","arguments":arguments}}]}}]})
}

fn finish(reason: &str) -> Value {
    json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]})
}

async fn collect(events: Vec<Result<SseEvent>>, tools: bool) -> Vec<Result<ProviderStreamEvent>> {
    adapt_chat_completion_stream(
        stream::iter(events).boxed(),
        tools.then(|| "kraai_nushell".into()),
        false,
    )
    .collect()
    .await
}

#[tokio::test]
async fn fragmented_arguments_preserve_script_and_drain_usage_after_call() -> Result<()> {
    let input = "# timeout=10sec\n'café </tool_call> \\\"'";
    let arguments = serde_json::to_string(&ScriptArguments {
        input: input.into(),
    })?;
    for split in arguments.char_indices().map(|(index, _)| index) {
        let (first, last) = arguments.split_at(split);
        let events = collect(vec![data(call(first)), data(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":last}}]}}]})), data(finish("tool_calls")), data(json!({"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":3}})), Ok(SseEvent::Done)], true).await.into_iter().collect::<Result<Vec<_>>>()?;
        assert!(
            matches!(events.get(2), Some(ProviderStreamEvent::Usage(usage)) if usage.total_tokens == 5)
        );
        assert!(
            matches!(events.first(), Some(ProviderStreamEvent::ScriptCall { call_id, name, input: script, arguments_error: None }) if call_id.as_str() == "call-1" && name == "kraai_nushell" && script == input)
        );
        assert!(matches!(
            events.get(1),
            Some(ProviderStreamEvent::Finished {
                stop_reason: StopReason::ToolCall
            })
        ));
        assert_eq!(events.len(), 3);
    }
    Ok(())
}

#[tokio::test]
async fn tool_fragments_without_indexes_preserve_the_single_call() -> Result<()> {
    let events = collect(
        vec![
            data(json!({"choices":[{"delta":{"tool_calls":[{"id":"call-1","type":"function","function":{"name":"kraai_nushell","arguments":"{\"input\":\""}}]}}]})),
            data(json!({"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"ls\"}"}}]}}]})),
            data(finish("tool_calls")),
            Ok(SseEvent::Done),
        ],
        true,
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    assert_eq!(events.len(), 2);
    assert!(
        matches!(events.first(), Some(ProviderStreamEvent::ScriptCall { call_id, name, input, arguments_error: None }) if call_id.as_str() == "call-1" && name == "kraai_nushell" && input == "ls")
    );
    Ok(())
}

#[tokio::test]
async fn multiple_calls_without_indexes_never_emit_a_script() {
    for chunks in [
        vec![json!({"choices":[{"delta":{"tool_calls":[
            {"id":"call-1","type":"function","function":{"name":"kraai_nushell","arguments":"{\"input\":\""}},
            {"function":{"arguments":"ls\"}"}}
        ]}}]})],
        vec![
            json!({"choices":[{"delta":{"tool_calls":[{"id":"call-1","type":"function","function":{"name":"kraai_nushell","arguments":"{\"input\":\""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"id":"call-2","function":{"arguments":"ls\"}"}}]}}]}),
        ],
    ] {
        let mut source: Vec<_> = chunks.into_iter().map(data).collect();
        source.extend([data(finish("tool_calls")), Ok(SseEvent::Done)]);
        let events = collect(source, true).await;
        assert!(events.iter().any(Result::is_err));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(ProviderStreamEvent::ScriptCall { .. })))
        );
    }
}

#[tokio::test]
async fn invalid_calls_keep_raw_arguments_and_their_error() -> Result<()> {
    let mut wrong_tool = call(r#"{"input":"ls"}"#);
    wrong_tool["choices"][0]["delta"]["tool_calls"][0]["function"]["name"] = json!("bash");
    for (chunk, name, arguments, error) in [
        (
            call(r#"{"command": "ls"}"#),
            "kraai_nushell",
            r#"{"command": "ls"}"#,
            "arguments must be a JSON object with one string field \"input\": unknown field `command`, expected `input` at line 1 column 10",
        ),
        (
            call(r#"{"input":"#),
            "kraai_nushell",
            r#"{"input":"#,
            "arguments must be a JSON object with one string field \"input\": EOF while parsing a value at line 1 column 9",
        ),
        (
            call(r#"{"input":"ls","extra":true}"#),
            "kraai_nushell",
            r#"{"input":"ls","extra":true}"#,
            "arguments must be a JSON object with one string field \"input\": unknown field `extra`, expected `input` at line 1 column 21",
        ),
        (
            wrong_tool,
            "bash",
            r#"{"input":"ls"}"#,
            "unknown tool 'bash'; the only tool is 'kraai_nushell'",
        ),
    ] {
        let events = collect(
            vec![data(chunk), data(finish("tool_calls")), Ok(SseEvent::Done)],
            true,
        )
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
        assert_eq!(
            events,
            [
                ProviderStreamEvent::ScriptCall {
                    call_id: ToolCallId::new("call-1"),
                    name: name.into(),
                    input: arguments.into(),
                    arguments_error: Some(error.into()),
                },
                ProviderStreamEvent::Finished {
                    stop_reason: StopReason::ToolCall
                },
            ]
        );
    }
    Ok(())
}

#[tokio::test]
async fn malformed_or_incomplete_calls_never_emit_a_script() {
    let good = r##"{"input":"# timeout=1sec\nls"}"##;
    let mut extra = call(good);
    extra["choices"][0]["delta"]["tool_calls"][0]["index"] = json!(1);
    let missing_id = |arguments: &str| {
        let mut chunk = call(arguments);
        chunk["choices"][0]["delta"]["tool_calls"][0]["id"] = Value::Null;
        vec![data(chunk), data(finish("tool_calls")), Ok(SseEvent::Done)]
    };
    for events in [
        missing_id(good),
        missing_id(r#"{"command":"ls"}"#),
        vec![data(call(good))],
        vec![data(call(good)), Ok(SseEvent::Done)],
        vec![data(call(good)), data(finish("stop")), Ok(SseEvent::Done)],
        vec![
            data(call(good)),
            data(extra),
            data(finish("tool_calls")),
            Ok(SseEvent::Done),
        ],
        vec![data(finish("tool_calls")), Ok(SseEvent::Done)],
    ] {
        let events = collect(events, true).await;
        assert!(events.iter().any(Result::is_err), "{events:?}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(ProviderStreamEvent::ScriptCall { .. }))),
            "{events:?}"
        );
    }
}

#[tokio::test]
async fn completed_call_survives_trailing_stream_failure() {
    for tail in [
        vec![],
        vec![Err(eyre!("connection lost"))],
        vec![data(json!({"error":{"message":"failed"}}))],
        vec![data(finish("tool_calls"))],
    ] {
        let mut source = vec![data(call(r#"{"input":"ls"}"#)), data(finish("tool_calls"))];
        source.extend(tail);
        let events = collect(source, true).await;
        assert!(
            matches!(events.first(), Some(Ok(ProviderStreamEvent::ScriptCall { input, .. })) if input == "ls")
        );
        assert!(matches!(
            events.get(1),
            Some(Ok(ProviderStreamEvent::Finished {
                stop_reason: StopReason::ToolCall
            }))
        ));
        assert!(events.get(2).is_some_and(Result::is_err));
        assert_eq!(events.len(), 3);
    }
}

#[tokio::test]
async fn completed_call_does_not_wait_for_trailing_usage_or_done() -> Result<()> {
    let source = stream::iter([data(call(r#"{"input":"ls"}"#)), data(finish("tool_calls"))])
        .chain(stream::pending())
        .boxed();
    let mut events = adapt_chat_completion_stream(source, Some("kraai_nushell".into()), false);
    let event = kraai_test_support::hang_guard(events.next()).await?;
    assert!(
        matches!(event, Some(Ok(ProviderStreamEvent::ScriptCall { input, .. })) if input == "ls")
    );
    Ok(())
}

#[tokio::test]
async fn disabled_tools_and_oversized_arguments_are_rejected() {
    for (arguments, tools) in [
        (r#"{"input":"ls"}"#.into(), false),
        ("x".repeat(MAX_TOOL_BYTES + 1), true),
    ] {
        let events = collect(
            vec![
                data(call(&arguments)),
                data(finish("tool_calls")),
                Ok(SseEvent::Done),
            ],
            tools,
        )
        .await;
        assert!(events.iter().any(Result::is_err));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(ProviderStreamEvent::ScriptCall { .. })))
        );
    }
}

#[tokio::test]
async fn text_usage_and_errors_survive_without_tools() -> Result<()> {
    let events = collect(vec![data(json!({"choices":[{"delta":{"content":"summary"},"finish_reason":"stop"}]})), data(json!({"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":45,"prompt_tokens_details":{"cached_tokens":20},"completion_tokens_details":{"reasoning_tokens":5}}})), Ok(SseEvent::Done)], false).await.into_iter().collect::<Result<Vec<_>>>()?;
    assert!(
        matches!(events.first(), Some(ProviderStreamEvent::TextDelta { delta, .. }) if delta == "summary")
    );
    assert!(matches!(
        events.get(1),
        Some(ProviderStreamEvent::Finished {
            stop_reason: StopReason::EndTurn
        })
    ));
    assert!(
        matches!(events.get(2), Some(ProviderStreamEvent::Usage(usage)) if usage.input_tokens == 100 && usage.output_tokens == 40 && usage.reasoning_tokens == 5 && usage.cache_read_tokens == 20)
    );
    for failure in [
        json!({"error":{"message":"rate limit"}}),
        finish("content_filter"),
        finish("function_call"),
        json!({"choices":[{"delta":{"refusal":"refused"}}]}),
    ] {
        let events = collect(vec![data(failure), Ok(SseEvent::Done)], false).await;
        assert!(events.iter().any(Result::is_err));
    }
    Ok(())
}

#[tokio::test]
async fn empty_repeated_final_choice_carries_usage_but_new_content_fails() -> Result<()> {
    let repeated = |delta: Value, reason: &str| json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}],"usage":{"prompt_tokens":8,"completion_tokens":2}});
    let events = collect(
        vec![
            data(json!({"choices":[{"delta":{"content":"Hi"},"finish_reason":"stop"}]})),
            data(repeated(json!({"content":"","role":"assistant"}), "stop")),
            Ok(SseEvent::Done),
        ],
        false,
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    assert!(matches!(
        events.get(1),
        Some(ProviderStreamEvent::Finished {
            stop_reason: StopReason::EndTurn
        })
    ));
    assert!(
        matches!(events.get(2), Some(ProviderStreamEvent::Usage(usage)) if usage.total_tokens == 10)
    );
    for (delta, reason) in [
        (json!({"content":"more"}), "stop"),
        (json!({"reasoning":"more"}), "stop"),
        (json!({}), "length"),
    ] {
        let events = collect(
            vec![
                data(finish("stop")),
                data(repeated(delta, reason)),
                Ok(SseEvent::Done),
            ],
            false,
        )
        .await;
        assert!(events.iter().any(Result::is_err));
    }
    Ok(())
}

#[tokio::test]
async fn termination_does_not_wait_for_transport_eof() -> Result<()> {
    for event in [
        data(finish("stop")),
        data(json!({"error":{"message":"failed"}})),
        Ok(SseEvent::Data("invalid".into())),
    ] {
        let source = stream::iter([event, Ok(SseEvent::Done)])
            .chain(stream::pending())
            .boxed();
        kraai_test_support::hang_guard(
            adapt_chat_completion_stream(source, None, false).collect::<Vec<_>>(),
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn reasoning_is_assembled_for_replay_before_the_tool_call() -> Result<()> {
    let events = collect(
        vec![
            data(json!({"choices":[{"delta":{"reasoning_content":"Inspect "}}]})),
            data(json!({"choices":[{"delta":{"reasoning_content":"the files."}}]})),
            data(call(r##"{"input":"# timeout=1sec\nls"}"##)),
            data(finish("tool_calls")),
            Ok(SseEvent::Done),
        ],
        true,
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    let deltas: String = events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::ReasoningDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "Inspect the files.");
    let rest: Vec<_> = events
        .iter()
        .filter(|event| !matches!(event, ProviderStreamEvent::ReasoningDelta { .. }))
        .collect();
    assert!(
        matches!(rest.first(), Some(ProviderStreamEvent::Reasoning { payload }) if payload == &json!({"reasoning_content":"Inspect the files."}))
    );
    assert!(matches!(
        rest.get(1),
        Some(ProviderStreamEvent::ScriptCall { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn structured_reasoning_fragments_survive_tool_continuation() -> Result<()> {
    let details = [
        json!({"type":"reasoning.text","index":0,"id":null,"text":"Inspect ","signature":null}),
        json!({"type":"reasoning.text","index":0,"id":"thought-1","text":"files","signature":"sig-complete","format":"anthropic-claude-v1"}),
        json!({"type":"reasoning.text","index":0,"text":".","signature":"sig-complete"}),
        json!({"type":"reasoning.text","index":1,"id":"thought-2","text":"Then test."}),
        json!({"type":"reasoning.summary","index":2,"summary":"Review "}),
        json!({"type":"reasoning.summary","index":2,"summary":"complete."}),
        json!({"type":"reasoning.encrypted","id":"encrypted-1","data":"opaque-one","format":"google-gemini-v1","provider_metadata":{"version":2}}),
        json!({"type":"reasoning.encrypted","id":"encrypted-1","data":"opaque-two","format":"google-gemini-v1"}),
    ];
    let expected = json!([
        {"type":"reasoning.text","index":0,"id":"thought-1","text":"Inspect files.","signature":"sig-complete","format":"anthropic-claude-v1"},
        {"type":"reasoning.text","index":1,"id":"thought-2","text":"Then test."},
        {"type":"reasoning.summary","index":2,"summary":"Review complete."},
        details[6], details[7],
    ]);
    let mut source = details
        .into_iter()
        .map(|detail| data(json!({"choices":[{"delta":{"reasoning_details":[detail]}}]})))
        .collect::<Vec<_>>();
    source.extend([
        data(call(r#"{"input":"ls"}"#)),
        data(finish("tool_calls")),
        Ok(SseEvent::Done),
    ]);
    let events = collect(source, true)
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    let Some(ProviderStreamEvent::Reasoning { payload }) = events.first() else {
        return Err(eyre!("missing structured reasoning"));
    };
    assert_eq!(payload, &json!({"reasoning_details":expected}));
    let provider_id = kraai_types::ProviderId::new("fixture");
    let wire = crate::messages::normalize_chat_messages(
        vec![
            kraai_types::ConversationItem::Assistant {
                items: vec![
                    kraai_types::AssistantItem::Reasoning {
                        provider_id: provider_id.clone(),
                        payload: payload.clone(),
                    },
                    kraai_types::AssistantItem::ScriptCall {
                        call_id: ToolCallId::new("call-1"),
                        name: "kraai_nushell".into(),
                        input: "ls".into(),
                        arguments_error: None,
                    },
                ],
            },
            kraai_types::ConversationItem::ScriptResult {
                outcome: kraai_execution_types::ScriptExecutionOutcome {
                    status: kraai_execution_types::ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                },
                call_id: ToolCallId::new("call-1"),
                output: "files".into(),
            },
        ],
        &kraai_provider_core::ResolvedImages::default(),
        &provider_id,
    )?;
    let wire = serde_json::to_value(wire)?;
    assert_eq!(wire[0]["reasoning_details"], expected);
    assert_eq!(wire[0]["tool_calls"][0]["id"], "call-1");
    assert_eq!(wire[1]["tool_call_id"], "call-1");
    Ok(())
}

#[tokio::test]
async fn structured_reasoning_limits_include_opaque_payloads() {
    let events = collect(vec![
        data(json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.encrypted","data":"x".repeat(4 * 1024 * 1024)}]}}]})),
        data(call(r#"{"input":"ls"}"#)),
        data(finish("tool_calls")),
        Ok(SseEvent::Done),
    ], true).await;
    assert_eq!(events.len(), 1);
    assert!(events.first().is_some_and(|event| {
        event
            .as_ref()
            .is_err_and(|error| error.to_string().contains("Reasoning exceeds size limit"))
    }));
}

#[tokio::test]
async fn length_finish_keeps_text_and_drops_the_truncated_call() -> Result<()> {
    let events = collect(
        vec![
            data(json!({"choices":[{"delta":{"content":"partial"}}]})),
            data(call(r#"{"input":"ls"#)),
            data(finish("length")),
            Ok(SseEvent::Done),
        ],
        true,
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    assert!(
        matches!(events.first(), Some(ProviderStreamEvent::TextDelta { delta, .. }) if delta == "partial")
    );
    assert!(matches!(
        events.get(1),
        Some(ProviderStreamEvent::Finished {
            stop_reason: StopReason::MaxOutputTokens
        })
    ));
    assert_eq!(events.len(), 2);
    Ok(())
}

#[tokio::test]
async fn several_indexed_calls_are_emitted_in_order() -> Result<()> {
    let mut second = call(r##"{"input":"# timeout=1sec\npwd"}"##);
    second["choices"][0]["delta"]["tool_calls"][0]["index"] = json!(1);
    second["choices"][0]["delta"]["tool_calls"][0]["id"] = json!("call-2");
    let events = collect(
        vec![
            data(call(r##"{"input":"# timeout=1sec\nls"}"##)),
            data(second),
            data(finish("tool_calls")),
            Ok(SseEvent::Done),
        ],
        true,
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    let calls: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::ScriptCall { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(calls, ["call-1", "call-2"]);
    Ok(())
}
