use std::collections::BTreeMap;
use std::time::Duration;

use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::{Harness, Reply, prompt};

fn configuration(provider: &str, updated: bool, extra_model: bool) -> String {
    let choices = if updated {
        r#"[{ id = "low", label = "Low" }]"#
    } else {
        r#"[{ id = "high", label = "High" }, { id = "low", label = "Low" }]"#
    };
    let removed = if updated {
        ""
    } else {
        r#"
[[model.options]]
id = "removed"
label = "Removed"
type = "boolean"
[model.options.binding]
type = "body"
path = "/custom/removed"
"#
    };
    let extra = if extra_model {
        r#"
[[model]]
id = "mock-model"
provider_id = "mock"
"#
    } else {
        ""
    };
    format!(
        r#"{provider}
[[model]]
id = "mock-alternate"
provider_id = "mock"

[[model.options]]
id = "effort"
label = "Effort"
type = "choice"
required = true
choices = {choices}
[model.options.binding]
type = "body"
path = "/reasoning_effort"

[[model.options]]
id = "retained"
label = "Retained"
type = "choice"
choices = [{{ id = "keep", label = "Keep" }}]
[model.options.binding]
type = "body"
path = "/custom/retained"
{removed}{extra}
"#
    )
}

async fn write_configuration(harness: &Harness, config: String) -> Result<()> {
    let temporary = harness.root.path().join("providers.toml.next");
    tokio::fs::write(&temporary, config).await?;
    tokio::fs::rename(temporary, harness.root.path().join("providers.toml")).await?;
    Ok(())
}

async fn configure(harness: &mut Harness) -> Result<String> {
    harness.stop().await?;
    let original = tokio::fs::read_to_string(harness.root.path().join("providers.toml")).await?;
    let (provider, _) = original
        .split_once("[[model]]")
        .ok_or_else(|| eyre!("missing fixture models"))?;
    write_configuration(harness, configuration(provider, false, true)).await?;
    harness
        .restart_with_selection(Some("mock"), Some("mock-alternate"))
        .await?;
    harness.initialize().await?;
    Ok(provider.to_owned())
}

fn option<'a>(message: &'a Value, id: &str) -> Option<&'a Value> {
    message
        .pointer("/params/update/configOptions")
        .or_else(|| message.pointer("/result/configOptions"))?
        .as_array()?
        .iter()
        .find(|option| option.get("id").and_then(Value::as_str) == Some(id))
}

async fn select(harness: &mut Harness, session: &str, effort: &str) -> Result<()> {
    for (request_id, (id, value)) in (10..).zip([
        ("effort", effort),
        ("retained", "keep"),
        ("removed", "true"),
    ]) {
        let responses = harness
            .request(
                request_id,
                "session/set_config_option",
                json!({"sessionId":session,"configId":format!("option:{id}"),"value":value}),
            )
            .await?;
        assert_eq!(
            responses
                .last()
                .and_then(|message| option(message, &format!("option:{id}")))
                .and_then(|option| option.get("currentValue")),
            Some(&json!(value))
        );
    }
    Ok(())
}

async fn updates(
    harness: &mut Harness,
    sessions: &[&str],
    matches: impl Fn(&Value) -> bool + Send + Sync,
) -> Result<BTreeMap<String, Value>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut updates = BTreeMap::new();
        while updates.len() < sessions.len() {
            let message = harness.read().await?;
            if message.pointer("/params/update/sessionUpdate")
                != Some(&json!("config_option_update"))
                || !matches(&message)
            {
                continue;
            }
            if let Some(session) = message.pointer("/params/sessionId").and_then(Value::as_str)
                && sessions.contains(&session)
            {
                updates.insert(session.to_owned(), message);
            }
        }
        Ok(updates)
    })
    .await?
}

fn has_updated_choices(message: &Value) -> bool {
    option(message, "option:effort").and_then(|option| option.get("options"))
        == Some(&json!([
            {"value":"", "name":"Choose a value"},
            {"value":"low", "name":"Low"},
        ]))
}

#[tokio::test]
async fn idle_new_and_loaded_sessions_receive_model_options_without_a_client_request() -> Result<()>
{
    let mut harness = Harness::new(vec![]).await?;
    let provider = configure(&mut harness).await?;
    let loaded_session = harness.session().await?;
    select(&mut harness, &loaded_session, "high").await?;
    harness.restart_with_selection(None, None).await?;
    harness.initialize().await?;
    let loaded = harness.load_session(&loaded_session, 20).await?;
    assert_eq!(
        loaded
            .last()
            .and_then(|message| option(message, "option:effort"))
            .and_then(|option| option.get("currentValue")),
        Some(&json!("high"))
    );
    let new_session = harness.session().await?;
    select(&mut harness, &new_session, "low").await?;
    let sessions = [loaded_session.as_str(), new_session.as_str()];
    write_configuration(&harness, configuration(&provider, true, true)).await?;
    let changed = updates(&mut harness, &sessions, has_updated_choices).await?;
    for (session, expected) in [(loaded_session.as_str(), ""), (new_session.as_str(), "low")] {
        let message = changed
            .get(session)
            .ok_or_else(|| eyre!("missing idle update for {session}"))?;
        assert_eq!(
            option(message, "option:effort").and_then(|option| option.get("currentValue")),
            Some(&json!(expected))
        );
        assert_eq!(
            option(message, "option:retained").and_then(|option| option.get("currentValue")),
            Some(&json!("keep"))
        );
        assert!(option(message, "option:removed").is_none());
    }
    write_configuration(&harness, configuration(&provider, true, false)).await?;
    let changed = updates(&mut harness, &sessions, |message| {
        option(message, "model")
            .and_then(|option| option.get("options"))
            .and_then(Value::as_array)
            .is_some_and(|choices| {
                choices.len() == 1
                    && choices.first().and_then(|choice| choice.get("value"))
                        == Some(&json!("4:mock:mock-alternate"))
            })
    })
    .await?;
    assert_eq!(changed.len(), 2);
    assert!(harness.provider_payloads.lock().await.is_empty());
    harness.stop().await
}

#[tokio::test]
async fn active_prompt_receives_model_options_before_its_completion() -> Result<()> {
    let mut harness = Harness::new(vec![Reply::Streaming]).await?;
    let provider = configure(&mut harness).await?;
    let session = harness.session().await?;
    select(&mut harness, &session, "high").await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":20,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    harness
        .until(|message| message.pointer("/params/update/content/text") == Some(&json!("partial")))
        .await?;
    write_configuration(&harness, configuration(&provider, true, true)).await?;
    let changed = updates(&mut harness, &[session.as_str()], has_updated_choices).await?;
    let message = changed
        .get(&session)
        .ok_or_else(|| eyre!("missing active session update"))?;
    assert_eq!(
        option(message, "option:effort").and_then(|option| option.get("currentValue")),
        Some(&json!(""))
    );
    assert_eq!(harness.provider_payloads.lock().await.len(), 1);
    harness
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
        .await?;
    let completed = harness
        .until(|message| message.get("id") == Some(&json!(20)))
        .await?;
    assert_eq!(
        completed
            .last()
            .and_then(|message| message.pointer("/result/stopReason")),
        Some(&json!("cancelled"))
    );
    assert_eq!(harness.provider_payloads.lock().await.len(), 1);
    harness.stop().await
}
