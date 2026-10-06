use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::{Harness, prompt};

async fn load(harness: &mut Harness, session: &str, id: u64) -> Result<Vec<Value>> {
    harness
        .request(
            id,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await
}

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
    let same_process = load(&mut harness, &session, 3).await?;
    assert_eq!(selected(&same_process), Some("4:mock:mock-alternate"));
    harness.restart_with_selection(None, None).await?;
    harness.initialize().await?;
    let restored = load(&mut harness, &session, 4).await?;
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
    let overridden = load(&mut harness, &session, 6).await?;
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
    let unavailable = load(&mut harness, &session, 3).await?;
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
    let overridden = load(&mut harness, &session, 4).await?;
    assert_eq!(selected(&overridden), Some("4:mock:mock-model"));
    harness.stop().await
}
