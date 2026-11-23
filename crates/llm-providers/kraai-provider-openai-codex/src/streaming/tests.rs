use super::*;
use futures::{StreamExt, stream};

#[tokio::test]
async fn encrypted_reasoning_is_preserved_before_its_tool_call() {
    let payload = serde_json::json!({"type":"reasoning","id":"rs-1","encrypted_content":"opaque-ciphertext", "summary":[], "status":"completed"});
    let source = stream::iter(vec![
        Ok(SseEvent::Data(serde_json::json!({"type":"response.output_item.added","item":{"type":"reasoning","id":"rs-1"}}).to_string())),
        Ok(SseEvent::Data(serde_json::json!({"type":"response.output_item.done","item":payload}).to_string())),
        Ok(SseEvent::Data(r#"{"type":"response.output_item.done","item":{"type":"custom_tool_call","call_id":"call-1","name":"kraai_nushell","input":"ls"}}"#.into())),
        Ok(SseEvent::Data(r#"{"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":20,"output_tokens_details":{"reasoning_tokens":15}}}}"#.into())),
    ]).boxed();
    let events = adapt_responses_stream(source).collect::<Vec<_>>().await;
    assert_eq!(events.len(), 4);
    assert!(
        matches!(events.first(), Some(Ok(ProviderStreamEvent::Reasoning { payload: actual })) if actual == &payload)
    );
    assert!(matches!(
        events.get(1),
        Some(Ok(ProviderStreamEvent::ScriptCall(_)))
    ));
    assert!(
        matches!(events.get(2), Some(Ok(ProviderStreamEvent::Usage(usage))) if usage.reasoning_tokens == 15 && usage.input_tokens == 100)
    );
    assert!(matches!(
        events.get(3),
        Some(Ok(ProviderStreamEvent::Finished {
            stop_reason: StopReason::ToolCall
        }))
    ));
}

#[tokio::test]
async fn max_output_token_incomplete_responses_finish_instead_of_failing() {
    let source = stream::iter(vec![
        Ok(SseEvent::Data(r#"{"type":"response.output_item.added","item":{"type":"message","id":"msg-1","phase":"final_answer"}}"#.into())),
        Ok(SseEvent::Data(r#"{"type":"response.output_text.delta","item_id":"msg-1","delta":"partial"}"#.into())),
        Ok(SseEvent::Data(r#"{"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":10,"output_tokens":5}}}"#.into())),
    ]).boxed();
    let events = adapt_responses_stream(source).collect::<Vec<_>>().await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(matches!(
        events.last(),
        Some(Ok(ProviderStreamEvent::Finished {
            stop_reason: StopReason::MaxOutputTokens
        }))
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(ProviderStreamEvent::TextDelta { delta, .. }) if delta == "partial"))
    );
}

#[tokio::test]
async fn reasoning_summaries_stream_as_display_deltas() {
    let source = stream::iter(vec![
        Ok(SseEvent::Data(r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs-1","delta":"Thinking"}"#.into())),
        Ok(SseEvent::Data(r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs-1","delta":""}"#.into())),
    ]).boxed();
    let events = adapt_responses_stream(source).collect::<Vec<_>>().await;
    let deltas: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Ok(ProviderStreamEvent::ReasoningDelta { item_id, delta }) => {
                Some((item_id.as_str(), delta.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(deltas, [("rs-1", "Thinking")]);
}

#[test]
fn normalize_usage_splits_cache_and_reasoning_tokens() {
    let usage = normalize_usage(ResponsesUsage {
        input_tokens: 120,
        output_tokens: 45,
        input_tokens_details: Some(crate::wire::ResponsesInputTokenDetails {
            cached_tokens: Some(20),
        }),
        output_tokens_details: Some(crate::wire::ResponsesOutputTokenDetails {
            reasoning_tokens: Some(5),
        }),
    })
    .expect("usage should normalize");

    assert_eq!(usage.total_tokens, 165);
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 40);
    assert_eq!(usage.reasoning_tokens, 5);
    assert_eq!(usage.cache_read_tokens, 20);
}

#[tokio::test]
async fn responses_stream_rejects_eof_before_completed() {
    let source = stream::iter(vec![
        Ok(SseEvent::Data(String::from(
            r#"{"type":"response.output_item.added","item":{"type":"message","id":"msg-1","phase":"commentary"}}"#,
        ))),
        Ok(SseEvent::Data(String::from(
            r#"{"type":"response.output_text.delta","item_id":"msg-1","delta":"partial"}"#,
        ))),
    ])
    .boxed();
    let events = adapt_responses_stream(source).collect::<Vec<_>>().await;

    assert!(matches!(
        events.first(),
        Some(Ok(ProviderStreamEvent::TextDelta { phase: AssistantPhase::Commentary, delta, .. })) if delta == "partial"
    ));
    assert!(events.get(1).is_some_and(Result::is_err));
}

#[tokio::test]
async fn responses_stream_rejects_completed_event_without_usage() {
    let source = stream::iter(vec![Ok(SseEvent::Data(String::from(
        r#"{"type":"response.completed","response":{}}"#,
    )))])
    .chain(stream::pending())
    .boxed();

    let event = kraai_test_support::hang_guard(adapt_responses_stream(source).next())
        .await
        .unwrap();

    assert!(event.is_some_and(|result| result.is_err()));
}

#[tokio::test]
async fn responses_stream_preserves_error_event_details_after_partial_text() {
    for code in [r#""server_error""#, "null"] {
        let source = stream::iter(vec![
            Ok(SseEvent::Data(String::from(
                r#"{"type":"response.output_text.delta","item_id":"msg-1","delta":"partial"}"#,
            ))),
            Ok(SseEvent::Data(format!(
                r#"{{"type":"error","code":{code},"message":"upstream failed","param":null,"sequence_number":2}}"#,
            ))),
        ])
        .boxed();
        let events = adapt_responses_stream(source).collect::<Vec<_>>().await;

        assert!(matches!(
            events.first(),
            Some(Ok(ProviderStreamEvent::TextDelta { delta, .. })) if delta == "partial"
        ));
        let error = events.get(1).unwrap().as_ref().unwrap_err().to_string();
        let detail = if code == "null" {
            "upstream failed"
        } else {
            "server_error: upstream failed"
        };
        assert_eq!(error, format!("OpenAI response stream failed: {detail}"));
        assert_eq!(events.len(), 2);
    }
}

#[tokio::test]
async fn responses_error_event_terminates_without_waiting_for_transport() {
    let source = stream::iter(vec![Ok(SseEvent::Data(String::from(
        r#"{"type":"error","message":"request failed"}"#,
    )))])
    .chain(stream::pending())
    .boxed();
    let mut events = adapt_responses_stream(source);
    let error = kraai_test_support::hang_guard(events.next())
        .await
        .expect("error event must not wait for transport EOF")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "OpenAI response stream failed: request failed"
    );
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn responses_stream_preserves_native_custom_call_identity_and_input() {
    let source = stream::iter(vec![
        Ok(SseEvent::Data(String::from(
            r##"{"type":"response.output_item.done","item":{"type":"custom_tool_call","id":"item-1","call_id":"call-123","name":"kraai_nushell","input":"# timeout=30sec\nls"}}"##,
        ))),
        Ok(SseEvent::Data(String::from(
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        ))),
    ])
    .boxed();

    let events = adapt_responses_stream(source).collect::<Vec<_>>().await;

    assert!(matches!(
        events.first(),
        Some(Ok(ProviderStreamEvent::ScriptCall(ScriptCall { call_id, name, input, arguments_error: None, rewrite: None })))
            if call_id.as_str() == "call-123"
                && name == "kraai_nushell"
                && input == "# timeout=30sec\nls"
    ));
    assert!(matches!(
        events.get(1),
        Some(Ok(ProviderStreamEvent::Usage(_)))
    ));
}
