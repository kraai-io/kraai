use std::sync::atomic::Ordering;

use color_eyre::eyre::Result;
use serde_json::{Value, json};

use crate::support::{Harness, PROFILE_ID, Reply, prompt, text_prompt};

#[tokio::test]
async fn continue_uses_selected_model_and_rejects_changes_during_active_prompts() -> Result<()> {
    let mut harness = Harness::new(vec![Reply::Streaming, "Resumed".into()]).await?;
    harness.install_profile().await?;
    harness.initialize().await?;
    let session = harness.session().await?;

    let empty = harness
        .request(2, "session/prompt", text_prompt(&session, "/continue"))
        .await?;
    assert_eq!(
        empty
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    let selected = harness
        .request(
            3,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"profile","value":PROFILE_ID}),
        )
        .await?;
    assert_eq!(
        selected
            .last()
            .and_then(|value| value.pointer("/result/configOptions/1/currentValue")),
        Some(&json!(PROFILE_ID))
    );
    assert!(selected.iter().any(|value| {
        value.pointer("/params/update/sessionUpdate") == Some(&json!("config_option_update"))
            && value.pointer("/params/update/configOptions/1/currentValue")
                == Some(&json!(PROFILE_ID))
    }));
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 0);

    harness
        .send(json!({"jsonrpc":"2.0","id":4,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    harness
        .until(|value| value.pointer("/params/update/content/text") == Some(&json!("partial")))
        .await?;
    let rejected = harness
        .request(
            5,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"profile","value":"coding"}),
        )
        .await?;
    assert!(
        rejected
            .last()
            .is_some_and(|value| value.get("error").is_some())
    );
    let undo = harness
        .request(8, "session/prompt", text_prompt(&session, "/undo"))
        .await?;
    assert!(
        undo.last()
            .is_some_and(|value| value.get("error").is_some())
    );
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 1);
    harness
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
        .await?;
    let cancelled = harness
        .until(|value| value.get("id") == Some(&json!(4)))
        .await?;
    assert_eq!(
        cancelled
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("cancelled"))
    );

    let model = harness
        .request(
            6,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"4:mock:mock-alternate"}),
        )
        .await?;
    assert_eq!(
        model
            .last()
            .and_then(|value| value.pointer("/result/configOptions/0/currentValue")),
        Some(&json!("4:mock:mock-alternate"))
    );
    let continued = harness
        .request(7, "session/prompt", text_prompt(&session, "/continue"))
        .await?;
    assert_eq!(
        continued
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    assert!(
        continued
            .iter()
            .any(|value| value.pointer("/params/update/content/text") == Some(&json!("Resumed")))
    );
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 2);
    let payloads = harness.provider_payloads.lock().await.clone();
    assert_eq!(payloads.len(), 2);
    for (payload, model) in payloads.into_iter().zip(["mock-model", "mock-alternate"]) {
        assert_eq!(payload.get("model"), Some(&json!(model)));
        let messages = payload
            .get("messages")
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(!messages.contains("/continue"));
        assert!(!messages.contains("/undo"));
    }
    harness.stop().await
}

#[tokio::test]
async fn empty_undo_and_rejected_commands_do_not_call_the_provider() -> Result<()> {
    let mut harness = Harness::new(vec![]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;

    let empty = harness
        .request(2, "session/prompt", text_prompt(&session, "/undo"))
        .await?;
    assert_eq!(
        empty
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    assert!(empty.iter().any(|value| {
        value
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    }));
    for (id, command) in [(3, "/undo extra"), (4, "/continue extra")] {
        let rejected = harness
            .request(id, "session/prompt", text_prompt(&session, command))
            .await?;
        assert!(
            rejected
                .last()
                .is_some_and(|value| value.get("error").is_some())
        );
        assert_eq!(harness.model_requests.load(Ordering::SeqCst), 0);
    }
    assert!(harness.provider_payloads.lock().await.is_empty());
    harness.stop().await
}
