use std::sync::atomic::Ordering;

use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::{Harness, PROFILE_ID, PROFILE_PROMPT, Reply, text_prompt};

#[tokio::test]
async fn undo_rewinds_persisted_context_without_reverting_files_or_the_selected_profile()
-> Result<()> {
    let mut harness = Harness::new(vec![
        "Kept answer".into(),
        Reply::Script(
            "# timeout=10sec permissions=no-sandbox\n'persistent' | save retained.txt".into(),
        ),
        "Discarded answer".into(),
        "Replacement answer".into(),
    ])
    .await?;
    harness.install_profile().await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    let kept = harness
        .request(2, "session/prompt", text_prompt(&session, "Keep this turn"))
        .await?;
    assert!(has_text(&kept, "Kept answer"));
    let selected = harness
        .request(
            3,
            "session/set_config_option",
            json!({
                "sessionId":session,"configId":"profile","value":PROFILE_ID
            }),
        )
        .await?;
    assert_eq!(
        selected
            .last()
            .and_then(|value| value.pointer("/result/configOptions/1/currentValue")),
        Some(&json!(PROFILE_ID))
    );
    harness.send(json!({"jsonrpc":"2.0","id":4,"method":"session/prompt","params":text_prompt(&session,"Discard this turn")})).await?;
    let pending = harness
        .until(|value| value.get("method") == Some(&json!("session/request_permission")))
        .await?;
    let approval = pending.last().ok_or_else(|| eyre!("missing approval"))?;
    harness.send(json!({"jsonrpc":"2.0","id":approval.get("id"),"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})).await?;
    let discarded = harness
        .until(|value| value.get("id") == Some(&json!(4)))
        .await?;
    assert!(has_text(&discarded, "Discarded answer"));
    assert_eq!(
        tokio::fs::read_to_string(harness.root.path().join("retained.txt")).await?,
        "persistent"
    );
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 3);

    let undone = harness
        .request(5, "session/prompt", text_prompt(&session, "/undo"))
        .await?;
    assert_eq!(
        undone
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    assert!(undone.iter().any(|value| {
        value
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    }));
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 3);
    assert_eq!(
        tokio::fs::read_to_string(harness.root.path().join("retained.txt")).await?,
        "persistent"
    );

    harness.restart().await?;
    harness.initialize().await?;
    let loaded = harness
        .request(
            6,
            "session/load",
            json!({
                "sessionId":session,"cwd":harness.root.path(),"mcpServers":[]
            }),
        )
        .await?;
    assert_eq!(
        loaded
            .last()
            .and_then(|value| value.pointer("/result/configOptions/1/currentValue")),
        Some(&json!(PROFILE_ID))
    );
    assert!(has_text(&loaded, "Keep this turn"));
    assert!(has_text(&loaded, "Kept answer"));
    let replay = serde_json::to_string(&loaded)?;
    for removed in [
        "Discard this turn",
        "Discarded answer",
        "retained.txt",
        "/undo",
    ] {
        assert!(
            !replay.contains(removed),
            "replayed discarded context: {removed}"
        );
    }
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 3);

    let replacement = harness
        .request(
            7,
            "session/prompt",
            text_prompt(&session, "Replacement task"),
        )
        .await?;
    assert!(has_text(&replacement, "Replacement answer"));
    let payloads = harness.provider_payloads.lock().await.clone();
    assert_eq!(payloads.len(), 4);
    let messages = payloads
        .last()
        .and_then(|value| value.get("messages"))
        .ok_or_else(|| eyre!("missing replacement messages"))?
        .to_string();
    for retained in [
        "Keep this turn",
        "Kept answer",
        "Replacement task",
        PROFILE_PROMPT,
    ] {
        assert!(
            messages.contains(retained),
            "missing retained context: {retained}"
        );
    }
    for removed in [
        "Discard this turn",
        "Discarded answer",
        "retained.txt",
        "/undo",
    ] {
        assert!(
            !messages.contains(removed),
            "sent discarded context: {removed}"
        );
    }
    harness.stop().await
}

fn has_text(messages: &[Value], text: &str) -> bool {
    messages
        .iter()
        .any(|value| value.pointer("/params/update/content/text") == Some(&json!(text)))
}
