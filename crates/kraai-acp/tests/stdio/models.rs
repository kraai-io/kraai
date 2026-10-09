use agent_client_protocol::schema::v1 as acp;
use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::{Harness, prompt};

fn selected(messages: &[Value]) -> Option<&str> {
    messages
        .last()?
        .pointer("/result/configOptions/0/currentValue")?
        .as_str()
}

#[tokio::test]
async fn saved_model_survives_restart_before_a_prompt_and_cli_overrides_are_explicit() -> Result<()>
{
    let mut harness =
        Harness::new(vec!["Saved model reply".into(), "Override reply".into()]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    let changed = harness
        .request(
            2,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"4:mock:mock-alternate"}),
        )
        .await?;
    assert_eq!(selected(&changed), Some("4:mock:mock-alternate"));
    let same_process = harness.load_session(&session, 3).await?;
    assert_eq!(selected(&same_process), Some("4:mock:mock-alternate"));
    harness.restart_with_selection(None, None).await?;
    harness.initialize().await?;
    let restored = harness.load_session(&session, 4).await?;
    assert_eq!(selected(&restored), Some("4:mock:mock-alternate"));
    let response = harness
        .request(5, "session/prompt", prompt(&session))
        .await?;
    assert_eq!(
        response
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    harness
        .restart_with_selection(Some("mock"), Some("mock-model"))
        .await?;
    harness.initialize().await?;
    let overridden = harness.load_session(&session, 6).await?;
    assert_eq!(selected(&overridden), Some("4:mock:mock-model"));
    harness
        .request(7, "session/prompt", prompt(&session))
        .await?;
    let payloads = harness.provider_payloads.lock().await.clone();
    assert_eq!(payloads.len(), 2);
    assert_eq!(
        payloads.first().and_then(|value| value.get("model")),
        Some(&json!("mock-alternate"))
    );
    assert_eq!(
        payloads.last().and_then(|value| value.get("model")),
        Some(&json!("mock-model"))
    );
    harness.stop().await
}

#[tokio::test]
async fn unavailable_saved_model_requires_explicit_replacement() -> Result<()> {
    let mut harness = Harness::new(vec![]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .request(
            2,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"4:mock:mock-alternate"}),
        )
        .await?;
    harness.stop().await?;
    let path = harness.root.path().join("providers.toml");
    let config = tokio::fs::read_to_string(&path).await?;
    let (retained, _) = config
        .rsplit_once("[[model]]")
        .ok_or_else(|| eyre!("missing alternate model"))?;
    tokio::fs::write(path, retained).await?;
    harness.restart_with_selection(None, None).await?;
    harness.initialize().await?;
    let unavailable = harness.load_session(&session, 3).await?;
    assert_eq!(
        unavailable
            .last()
            .and_then(|value| value.pointer("/error/code")),
        Some(&json!(-32602))
    );
    assert!(unavailable.last().is_some_and(|value| {
        value
            .to_string()
            .contains("Saved model is no longer configured")
    }));
    harness
        .restart_with_selection(Some("mock"), Some("mock-model"))
        .await?;
    harness.initialize().await?;
    let overridden = harness.load_session(&session, 4).await?;
    assert_eq!(selected(&overridden), Some("4:mock:mock-model"));
    harness.stop().await
}

#[tokio::test]
async fn custom_options_persist_before_prompts_and_survive_reselection_and_restart() -> Result<()> {
    let mut harness = Harness::new(vec!["Selected options reply".into()]).await?;
    harness.stop().await?;
    let path = harness.root.path().join("providers.toml");
    let mut config = tokio::fs::read_to_string(&path).await?;
    config.push_str(
        r#"
[[model.options]]
id = "effort"
label = "Effort"
type = "choice"
required = true
[[model.options.choices]]
id = "high"
label = "High"
[model.options.choices.patch.body]
reasoning_effort = "high"
[[model.options.choices]]
id = "low"
label = "Low"
[model.options.choices.patch.body]
reasoning_effort = "low"

[[model.options]]
id = "feature_enabled"
label = "Feature enabled"
type = "boolean"
required = true
[model.options.binding]
type = "body"
path = "/custom/feature_enabled"

[[model.options]]
id = "token_budget"
label = "Token budget"
type = "integer"
required = true
min = 1
max = 10000
[model.options.binding]
type = "body"
path = "/custom/token_budget"
"#,
    );
    tokio::fs::write(&path, config).await?;
    harness
        .restart_with_selection(Some("mock"), Some("mock-alternate"))
        .await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .request(
            10,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"option:effort","value":"high"}),
        )
        .await?;
    let enabled = harness
        .request(
            11,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"option:feature_enabled","value":"true"}),
        )
        .await?;
    assert_eq!(
        option_value(&enabled, "option:feature_enabled"),
        Some(&json!(true))
    );
    let disabled = harness
        .request(
            12,
            "session/set_config_option",
            serde_json::to_value(acp::SetSessionConfigOptionRequest::new(
                session.clone(),
                "option:feature_enabled",
                false,
            ))?,
        )
        .await?;
    assert_eq!(
        option_value(&disabled, "option:feature_enabled"),
        Some(&json!(false))
    );
    let configured = harness
        .request(
            13,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"option:token_budget","value":"4096"}),
        )
        .await?;
    assert_selected_options(&configured);
    let reselected = harness
        .request(
            14,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"4:mock:mock-alternate"}),
        )
        .await?;
    assert_selected_options(&reselected);
    assert!(harness.provider_payloads.lock().await.is_empty());
    harness.restart_with_selection(None, None).await?;
    harness.initialize().await?;
    let restored = harness.load_session(&session, 15).await?;
    assert_selected_options(&restored);
    assert!(harness.provider_payloads.lock().await.is_empty());
    harness
        .request(16, "session/prompt", prompt(&session))
        .await?;
    {
        let payloads = harness.provider_payloads.lock().await.clone();
        assert_eq!(payloads.len(), 1);
        let payload = payloads.first().ok_or_else(|| eyre!("missing request"))?;
        assert_eq!(payload.get("model"), Some(&json!("mock-alternate")));
        assert_eq!(payload.get("reasoning_effort"), Some(&json!("high")));
        assert_eq!(
            payload.pointer("/custom/feature_enabled"),
            Some(&json!(false))
        );
        assert_eq!(payload.pointer("/custom/token_budget"), Some(&json!(4096)));
    }
    let changed = harness
        .request(
            17,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"4:mock:mock-model"}),
        )
        .await?;
    assert_eq!(selected(&changed), Some("4:mock:mock-model"));
    assert!(option_value(&changed, "option:effort").is_none());
    let returned = harness
        .request(
            18,
            "session/set_config_option",
            json!({"sessionId":session,"configId":"model","value":"4:mock:mock-alternate"}),
        )
        .await?;
    assert_eq!(selected(&returned), Some("4:mock:mock-alternate"));
    for id in [
        "option:effort",
        "option:feature_enabled",
        "option:token_budget",
    ] {
        assert_eq!(option_value(&returned, id), Some(&json!("")));
    }
    let rejected = harness
        .request(19, "session/prompt", prompt(&session))
        .await?;
    assert!(
        rejected
            .last()
            .is_some_and(|value| value.get("error").is_some())
    );
    assert_eq!(harness.provider_payloads.lock().await.len(), 1);
    harness.stop().await
}

fn assert_selected_options(messages: &[Value]) {
    assert_eq!(selected(messages), Some("4:mock:mock-alternate"));
    for (id, value) in [
        ("option:effort", json!("high")),
        ("option:feature_enabled", json!(false)),
        ("option:token_budget", json!("4096")),
    ] {
        assert_eq!(option_value(messages, id), Some(&value));
    }
}

fn option_value<'a>(messages: &'a [Value], id: &str) -> Option<&'a Value> {
    messages
        .last()?
        .pointer("/result/configOptions")?
        .as_array()?
        .iter()
        .find(|option| option.get("id").and_then(Value::as_str) == Some(id))?
        .get("currentValue")
}
