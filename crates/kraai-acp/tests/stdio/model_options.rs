use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::{Harness, prompt, text_prompt};

async fn configure(harness: &mut Harness, options: &str) -> Result<()> {
    harness.stop().await?;
    let path = harness.root.path().join("providers.toml");
    let config = tokio::fs::read_to_string(&path).await?;
    let base = config
        .split_once("[[model.options]]")
        .map_or(config.as_str(), |(base, _)| base);
    tokio::fs::write(path, format!("{base}{options}"))
        .await
        .map_err(Into::into)
}

async fn set(
    harness: &mut Harness,
    session: &str,
    id: u64,
    option: &str,
    value: &str,
) -> Result<Vec<Value>> {
    harness
        .request(
            id,
            "session/set_config_option",
            json!({"sessionId":session,"configId":format!("option:{option}"),"value":value}),
        )
        .await
}

fn option<'a>(messages: &'a [Value], id: &str) -> Option<&'a Value> {
    messages.iter().rev().find_map(|message| {
        message
            .pointer("/result/configOptions")
            .or_else(|| message.pointer("/params/update/configOptions"))?
            .as_array()?
            .iter()
            .find(|option| option.get("id").and_then(Value::as_str) == Some(id))
    })
}

fn value<'a>(messages: &'a [Value], id: &str) -> Option<&'a Value> {
    option(messages, id)?.get("currentValue")
}

const OPTIONAL_OPTIONS: &str = r#"
[[model.options]]
id = "effort"
label = "Effort"
type = "choice"
required = true
choices = [{ id = "high", label = "High" }, { id = "low", label = "Low" }]
[model.options.binding]
type = "body"
path = "/reasoning_effort"

[[model.options]]
id = "processing"
label = "Processing"
type = "boolean"
[model.options.binding]
type = "body"
path = "/custom/processing"

[[model.options]]
id = "budget"
label = "Budget"
type = "integer"
min = 0
max = 100
active_when = { option = "processing", value = true }
[model.options.binding]
type = "body"
path = "/custom/budget"

[[model.options]]
id = "style"
label = "Style"
type = "choice"
choices = [{ id = "fast", label = "Fast" }, { id = "slow", label = "Slow" }]
[model.options.binding]
type = "body"
path = "/custom/style"

[[model.options]]
id = "limit"
label = "Limit"
type = "integer"
min = 0
max = 100
[model.options.binding]
type = "body"
path = "/custom/limit"
"#;

#[tokio::test]
async fn optional_controls_clear_and_persist_omission_across_restart() -> Result<()> {
    let mut harness = Harness::new(vec!["Configured".into(), "Cleared".into()]).await?;
    configure(&mut harness, OPTIONAL_OPTIONS).await?;
    harness
        .restart_with_selection(Some("mock"), Some("mock-alternate"))
        .await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    for (request_id, (id, input)) in (10..).zip([
        ("effort", "high"),
        ("processing", "true"),
        ("budget", "80"),
        ("style", "fast"),
        ("limit", "0"),
    ]) {
        let configured = set(&mut harness, &session, request_id, id, input).await?;
        assert_eq!(
            value(&configured, &format!("option:{id}")),
            Some(&json!(input))
        );
    }
    let configured = harness.load_session(&session, 20).await?;
    for id in ["processing", "style", "limit"] {
        let control =
            option(&configured, &format!("option:{id}")).ok_or_else(|| eyre!("missing {id}"))?;
        assert_eq!(control.get("type"), Some(&json!("select")));
        assert_eq!(control.pointer("/options/0/value"), Some(&json!("")));
        assert_eq!(control.pointer("/options/0/name"), Some(&json!("Unset")));
    }
    harness
        .request(21, "session/prompt", prompt(&session))
        .await?;
    let cleared = set(&mut harness, &session, 22, "processing", "").await?;
    assert_eq!(value(&cleared, "option:processing"), Some(&json!("")));
    assert!(option(&cleared, "option:budget").is_none());
    let cleared = harness
        .request(
            23,
            "session/prompt",
            text_prompt(&session, "/option style --clear"),
        )
        .await?;
    assert_eq!(value(&cleared, "option:style"), Some(&json!("")));
    let cleared = set(&mut harness, &session, 24, "limit", "").await?;
    assert_eq!(value(&cleared, "option:limit"), Some(&json!("")));
    let rejected = set(&mut harness, &session, 25, "effort", "").await?;
    assert_eq!(
        rejected
            .last()
            .and_then(|message| message.pointer("/error/code")),
        Some(&json!(-32602))
    );
    assert_eq!(harness.provider_payloads.lock().await.len(), 1);
    harness.restart_with_selection(None, None).await?;
    harness.initialize().await?;
    let restored = harness.load_session(&session, 26).await?;
    assert_eq!(value(&restored, "option:effort"), Some(&json!("high")));
    for id in ["processing", "style", "limit"] {
        assert_eq!(value(&restored, &format!("option:{id}")), Some(&json!("")));
    }
    assert!(option(&restored, "option:budget").is_none());
    harness
        .request(27, "session/prompt", prompt(&session))
        .await?;
    {
        let payloads = harness.provider_payloads.lock().await.clone();
        assert_eq!(payloads.len(), 2);
        let first = payloads
            .first()
            .ok_or_else(|| eyre!("missing configured request"))?;
        assert_eq!(first.pointer("/custom/processing"), Some(&json!(true)));
        assert_eq!(first.pointer("/custom/budget"), Some(&json!(80)));
        assert_eq!(first.pointer("/custom/style"), Some(&json!("fast")));
        assert_eq!(first.pointer("/custom/limit"), Some(&json!(0)));
        let last = payloads
            .last()
            .ok_or_else(|| eyre!("missing cleared request"))?;
        assert_eq!(last.get("reasoning_effort"), Some(&json!("high")));
        assert!(last.get("custom").is_none());
    }
    harness.stop().await
}

fn changing_options(updated: bool) -> String {
    let (effort, maximum, kind, choices, condition) = if updated {
        (
            "low",
            50,
            "choice",
            "\nchoices = [{ id = \"on\", label = \"On\" }]",
            "\"on\"",
        )
    } else {
        ("high", 100, "boolean", "", "true")
    };
    let removed = if updated {
        ""
    } else {
        r#"
[[model.options]]
id = "removed"
label = "Removed"
type = "integer"
[model.options.binding]
type = "body"
path = "/custom/removed"
"#
    };
    format!(
        r#"
[[model.options]]
id = "effort"
label = "Effort"
type = "choice"
required = true
choices = [{{ id = "{effort}", label = "{effort}" }}]
[model.options.binding]
type = "body"
path = "/reasoning_effort"

[[model.options]]
id = "budget"
label = "Budget"
type = "integer"
min = 0
max = {maximum}
[model.options.binding]
type = "body"
path = "/custom/budget"

[[model.options]]
id = "gate"
label = "Gate"
type = "{kind}"{choices}
[model.options.binding]
type = "body"
path = "/custom/gate"

[[model.options]]
id = "dependent"
label = "Dependent"
type = "integer"
active_when = {{ option = "gate", value = {condition} }}
[model.options.binding]
type = "body"
path = "/custom/dependent"

[[model.options]]
id = "retained"
label = "Retained"
type = "integer"
[model.options.binding]
type = "body"
path = "/custom/retained"
{removed}
"#
    )
}

#[tokio::test]
async fn saved_options_reconcile_metadata_drift_without_accepting_invalid_explicit_overrides()
-> Result<()> {
    let mut harness = Harness::new(vec!["Reconciled".into()]).await?;
    configure(&mut harness, &changing_options(false)).await?;
    harness
        .restart_with_selection(Some("mock"), Some("mock-alternate"))
        .await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    for (request_id, (id, input)) in (10..).zip([
        ("effort", "high"),
        ("budget", "80"),
        ("gate", "true"),
        ("dependent", "10"),
        ("retained", "0"),
        ("removed", "7"),
    ]) {
        let configured = set(&mut harness, &session, request_id, id, input).await?;
        assert!(
            configured
                .last()
                .is_some_and(|message| message.get("result").is_some())
        );
    }
    configure(&mut harness, &changing_options(true)).await?;
    harness.restart_with_selection(None, None).await?;
    harness.initialize().await?;
    let restored = harness.load_session(&session, 20).await?;
    assert_eq!(
        restored
            .last()
            .and_then(|message| message.pointer("/result/configOptions/0/currentValue")),
        Some(&json!("4:mock:mock-alternate"))
    );
    for id in ["effort", "budget", "gate"] {
        assert_eq!(value(&restored, &format!("option:{id}")), Some(&json!("")));
    }
    assert_eq!(value(&restored, "option:retained"), Some(&json!("0")));
    for id in ["dependent", "removed"] {
        assert!(option(&restored, &format!("option:{id}")).is_none());
    }
    let rejected = harness
        .request(21, "session/prompt", prompt(&session))
        .await?;
    assert!(
        rejected
            .last()
            .is_some_and(|message| message.get("error").is_some())
    );
    for invalid in ["effort=high", "budget=80", "gate=true"] {
        harness.restart_with_options(None, None, &[invalid]).await?;
        harness.initialize().await?;
        let rejected = harness.load_session(&session, 22).await?;
        assert_eq!(
            rejected
                .last()
                .and_then(|message| message.pointer("/error/code")),
            Some(&json!(-32602)),
            "{invalid}: {rejected:?}"
        );
    }
    assert!(harness.provider_payloads.lock().await.is_empty());
    harness
        .restart_with_options(None, None, &["effort=low"])
        .await?;
    harness.initialize().await?;
    let restored = harness.load_session(&session, 23).await?;
    assert_eq!(value(&restored, "option:effort"), Some(&json!("low")));
    assert_eq!(value(&restored, "option:retained"), Some(&json!("0")));
    harness
        .request(24, "session/prompt", prompt(&session))
        .await?;
    {
        let payloads = harness.provider_payloads.lock().await.clone();
        assert_eq!(payloads.len(), 1);
        let payload = payloads
            .first()
            .ok_or_else(|| eyre!("missing reconciled request"))?;
        assert_eq!(payload.get("reasoning_effort"), Some(&json!("low")));
        assert_eq!(payload.get("custom"), Some(&json!({"retained":0})));
    }
    harness.stop().await
}
