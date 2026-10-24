use std::sync::atomic::Ordering;

use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::{Harness, Reply, text_prompt};

#[tokio::test]
async fn output_limit_stop_is_shown_in_the_truncated_message() -> Result<()> {
    let mut harness = Harness::new(vec![Reply::Truncated("Cut off".into())]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;

    let response = harness
        .request(2, "session/prompt", text_prompt(&session, "Run the task"))
        .await?;
    assert_eq!(
        response
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    let message_id = |text: &dyn Fn(&str) -> bool| {
        response.iter().find_map(|value| {
            value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                .is_some_and(text)
                .then(|| value.pointer("/params/update/messageId").cloned())
                .flatten()
        })
    };
    let truncated = message_id(&|text| text == "Cut off");
    let notice = message_id(&|text| text.contains("output token limit"));
    assert!(truncated.is_some());
    assert_eq!(notice, truncated);
    harness.stop().await
}

#[tokio::test]
async fn compaction_before_any_reply_text_is_shown_on_its_own() -> Result<()> {
    let mut harness = Harness::new(vec!["First".into(), "Summary".into(), "Second".into()]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .request(2, "session/prompt", text_prompt(&session, "Start"))
        .await?;
    let tokens = harness
        .provider_payloads
        .lock()
        .await
        .first()
        .ok_or_else(|| eyre!("missing first model request"))?
        .to_string()
        .len()
        / 4;
    let config = harness.root.path().join("providers.toml");
    let providers = tokio::fs::read_to_string(&config).await?.replacen(
        "id = \"mock-model\"\n",
        &format!("id = \"mock-model\"\nmax_context = {}\n", tokens * 4),
        1,
    );
    tokio::fs::write(&config, providers).await?;
    harness.restart().await?;
    harness.initialize().await?;
    harness.load_session(&session, 3).await?;

    let response = harness
        .request(
            4,
            "session/prompt",
            text_prompt(&session, &"filler ".repeat(tokens * 10 / 7)),
        )
        .await?;
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 3);
    let update = |text: &str| {
        response.iter().find_map(|value| {
            value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                .is_some_and(|chunk| chunk.contains(text))
                .then(|| value.pointer("/params/update"))
                .flatten()
        })
    };
    assert!(
        update("Second")
            .and_then(|reply| reply.get("messageId"))
            .is_some()
    );
    for notice in ["Compacting", "compacted"] {
        let notice = update(notice).ok_or_else(|| eyre!("missing notice: {notice}"))?;
        assert_eq!(notice.get("messageId"), None);
    }
    harness.stop().await
}
