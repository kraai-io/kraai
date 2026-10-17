use color_eyre::eyre::Result;
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
