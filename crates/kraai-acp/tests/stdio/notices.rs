use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use crate::support::{Harness, Reply, text_prompt};

fn updates<'a>(response: &'a [Value], kind: &'a str) -> impl Iterator<Item = &'a Value> {
    response
        .iter()
        .filter_map(|value| value.pointer("/params/update"))
        .filter(move |update| update.get("sessionUpdate") == Some(&json!(kind)))
}

fn notice<'a>(response: &'a [Value], text: &str) -> Option<&'a Value> {
    updates(response, "tool_call").find(|update| {
        update
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| title.contains(text))
    })
}

#[tokio::test]
async fn output_limit_stop_is_shown_as_a_completed_notice() {
    let mut harness = Harness::new(vec![Reply::Truncated("Cut off".into())]).await;
    harness.initialize().await;
    let session = harness.session().await;

    let response = harness
        .request(2, "session/prompt", text_prompt(&session, "Run the task"))
        .await;
    assert_eq!(
        response
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    let reply = updates(&response, "agent_message_chunk")
        .filter_map(|update| update.pointer("/content/text").and_then(Value::as_str))
        .collect::<String>();
    assert_eq!(reply, "Cut off");
    let notice = notice(&response, "output token limit").expect("missing notice");
    assert_eq!(notice.get("status"), Some(&json!("completed")));
    harness.stop().await
}

#[tokio::test]
async fn compaction_is_shown_as_notices() {
    let mut harness = Harness::new(vec!["First".into(), "Summary".into(), "Second".into()]).await;
    harness.initialize().await;
    let session = harness.session().await;
    harness
        .request(2, "session/prompt", text_prompt(&session, "Start"))
        .await;
    let tokens = harness
        .provider_payloads
        .lock()
        .await
        .first()
        .expect("missing first model request")
        .to_string()
        .len()
        / 4;
    let config = harness.root.path().join("providers.toml");
    let providers = tokio::fs::read_to_string(&config).await.unwrap().replacen(
        "id = \"mock-model\"\n",
        &format!("id = \"mock-model\"\nmax_context = {}\n", tokens * 4),
        1,
    );
    tokio::fs::write(&config, providers).await.unwrap();
    harness.restart().await;
    harness.initialize().await;
    harness.load_session(&session, 3).await;

    let response = harness
        .request(
            4,
            "session/prompt",
            text_prompt(&session, &"filler ".repeat(tokens * 10 / 7)),
        )
        .await;
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 3);
    assert!(
        updates(&response, "agent_message_chunk")
            .any(|update| update.pointer("/content/text") == Some(&json!("Second")))
    );
    for text in ["Compacting", "compacted"] {
        notice(&response, text).unwrap_or_else(|| panic!("missing notice: {text}"));
    }
    harness.stop().await
}

#[tokio::test]
async fn rate_limit_retries_show_one_row_that_completes_when_the_reply_starts() {
    let mut harness = Harness::new(vec![Reply::RateLimited, "Done".into()]).await;
    harness.initialize().await;
    let session = harness.session().await;

    let response = harness
        .request(2, "session/prompt", text_prompt(&session, "Start"))
        .await;
    let rows = updates(&response, "tool_call").collect::<Vec<_>>();
    let [row] = rows.as_slice() else {
        panic!("expected one retry row, got {rows:?}");
    };
    assert_eq!(row.get("status"), Some(&json!("in_progress")));
    assert!(
        row.get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| title.contains("429"))
    );
    let position = |predicate: &dyn Fn(&Value) -> bool| {
        response
            .iter()
            .filter_map(|value| value.pointer("/params/update"))
            .position(predicate)
    };
    let completed = position(&|update| {
        update.get("toolCallId") == row.get("toolCallId")
            && update.get("status") == Some(&json!("completed"))
    })
    .expect("retry row never completed");
    let reply = position(&|update| update.pointer("/content/text") == Some(&json!("Done")))
        .expect("missing reply");
    assert!(completed < reply);
    harness.stop().await
}
