#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert after fallible runtime operations"
)]

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    routing::{get, post},
};
use color_eyre::eyre::{Result, eyre};
use kraai_runtime::{Event, RuntimeBuilder, RuntimeHandle, RuntimeStartupState};
use kraai_types::{
    ModelId, ModelOptionDefinition, ModelOptionValue, ModelOptionValues, ModelSelection, ProviderId,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::task::AbortOnDropHandle;

use super::*;

struct Fixture {
    runtime: RuntimeHandle,
    session: String,
    payloads: Arc<Mutex<Vec<Value>>>,
    _server: AbortOnDropHandle<()>,
    _root: tempfile::TempDir,
}

impl Fixture {
    async fn new(
        definitions: Vec<ModelOptionDefinition>,
        values: ModelOptionValues,
    ) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let received = payloads.clone();
        let router = Router::new()
            .route("/models", get(async || axum::Json(json!({"data":[{"id":"model"}]}))))
            .route("/chat/completions", post(move |axum::Json(payload): axum::Json<Value>| {
                let received = received.clone();
                async move {
                    received.lock().await.push(payload);
                    ([("content-type", "text/event-stream")], "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = AbortOnDropHandle::new(tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        }));
        let config = root.path().join("providers.toml");
        tokio::fs::write(
            &config,
            format!(
                r#"
[[provider]]
id = "mock"
type = "openai-chat-completions"
base_url = "http://{address}"
api_key = "test"
only_listed_models = true

[[model]]
id = "model"
provider_id = "mock"
"#
            ),
        )
        .await?;
        let runtime = RuntimeBuilder::new()
            .storage_root(root.path().join("state"))
            .provider_config_path(config)
            .mcp_config_path(root.path().join("mcp.toml"))
            .build_on(&tokio::runtime::Handle::current());
        assert_eq!(
            runtime.wait_for_startup().await?,
            RuntimeStartupState::Ready
        );
        let session = runtime.create_session().await?;
        let fixture = Self {
            runtime,
            session,
            payloads,
            _server: server,
            _root: root,
        };
        fixture.update(definitions).await?;
        fixture
            .runtime
            .set_session_model(fixture.session.clone(), selection(values))
            .await?;
        Ok(fixture)
    }

    async fn update(&self, definitions: Vec<ModelOptionDefinition>) -> Result<()> {
        let mut settings = self.runtime.get_settings().await?;
        settings
            .models
            .first_mut()
            .ok_or_else(|| eyre!("missing configured model"))?
            .options = definitions;
        self.runtime.save_settings(settings).await?;
        Ok(())
    }
}

fn selection(options: ModelOptionValues) -> ModelSelection {
    ModelSelection {
        provider_id: ProviderId::new("mock"),
        model_id: ModelId::new("model"),
        options,
    }
}

fn changing_definitions(effort: &str, maximum: i64) -> Result<Vec<ModelOptionDefinition>> {
    Ok(serde_json::from_value(json!([
        {"id":"effort","label":"Effort","required":true,"type":"choice",
            "choices":[{"id":effort,"label":effort}],"binding":{"type":"body","path":"/reasoning_effort"}},
        {"id":"budget","label":"Budget","type":"integer","min":0,"max":maximum,
            "binding":{"type":"body","path":"/budget"}},
        {"id":"retained","label":"Retained","type":"boolean","binding":{"type":"body","path":"/retained"}}
    ]))?)
}

#[tokio::test]
async fn active_session_metadata_changes_reconcile_controls_and_allow_individual_edits()
-> Result<()> {
    let old_values = ModelOptionValues::from([
        ("effort".into(), ModelOptionValue::Choice("high".into())),
        ("budget".into(), ModelOptionValue::Integer(80)),
        ("retained".into(), ModelOptionValue::Boolean(false)),
    ]);
    let fixture = Fixture::new(changing_definitions("high", 100)?, old_values.clone()).await?;
    let reselected = fixture.runtime.create_session().await?;
    fixture
        .runtime
        .set_session_model(reselected.clone(), selection(old_values.clone()))
        .await?;
    fixture.update(changing_definitions("low", 50)?).await?;
    let descriptors = options(&fixture.runtime, &fixture.session).await?;
    for id in ["option:effort", "option:budget"] {
        let descriptor = descriptors
            .iter()
            .find(|option| option.id.0.as_ref() == id)
            .ok_or_else(|| eyre!("missing option {id}"))?;
        assert_eq!(
            serde_json::to_value(descriptor)?.get("currentValue"),
            Some(&json!(""))
        );
    }
    let selected = crate::session::selected_model(
        &fixture.runtime,
        &acp::SessionId::new(fixture.session.clone()),
    )
    .await?;
    assert_eq!(
        selected.options,
        ModelOptionValues::from([("retained".into(), ModelOptionValue::Boolean(false))])
    );
    assert!(
        fixture
            .runtime
            .send_message(
                fixture.session.clone(),
                "blocked".into(),
                selected.model,
                selected.provider,
                selected.options
            )
            .await
            .is_err()
    );
    assert!(fixture.payloads.lock().await.is_empty());
    assert!(
        set_model_option(&fixture.runtime, &fixture.session, "effort", "high")
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .runtime
            .get_session_model(fixture.session.clone())
            .await?
            .map(|model| model.options),
        Some(old_values)
    );
    set_model_option(&fixture.runtime, &fixture.session, "effort", "low").await?;
    set_model_option(&fixture.runtime, &fixture.session, "budget", "40").await?;
    assert_eq!(
        fixture
            .runtime
            .get_session_model(fixture.session.clone())
            .await?
            .map(|model| model.options),
        Some(ModelOptionValues::from([
            ("effort".into(), ModelOptionValue::Choice("low".into())),
            ("budget".into(), ModelOptionValue::Integer(40)),
            ("retained".into(), ModelOptionValue::Boolean(false)),
        ]))
    );
    set(
        &fixture.runtime,
        &reselected,
        "model",
        acp::SessionConfigOptionValue::ValueId {
            value: "4:mock:model".into(),
        },
    )
    .await?;
    assert_eq!(
        fixture
            .runtime
            .get_session_model(reselected)
            .await?
            .map(|model| model.options),
        Some(ModelOptionValues::from([(
            "retained".into(),
            ModelOptionValue::Boolean(false)
        )]))
    );
    fixture.runtime.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn removing_the_final_optional_setting_does_not_block_the_next_prompt() -> Result<()> {
    let definitions = serde_json::from_value(json!([{
        "id":"removed","label":"Removed","type":"boolean","binding":{"type":"body","path":"/removed"}
    }]))?;
    let fixture = Fixture::new(
        definitions,
        ModelOptionValues::from([("removed".into(), ModelOptionValue::Boolean(true))]),
    )
    .await?;
    fixture.update(Vec::new()).await?;
    let selected = crate::session::selected_model(
        &fixture.runtime,
        &acp::SessionId::new(fixture.session.clone()),
    )
    .await?;
    assert!(selected.options.is_empty());
    let mut events = fixture.runtime.subscribe_session(&fixture.session);
    fixture
        .runtime
        .send_message(
            fixture.session.clone(),
            "continue".into(),
            selected.model,
            selected.provider,
            selected.options,
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await?.event {
                Event::TurnCompleted { .. } => return Ok::<_, color_eyre::Report>(()),
                Event::StreamError { error, .. } => return Err(eyre!(error)),
                _ => {}
            }
        }
    })
    .await??;
    let payloads = fixture.payloads.lock().await.clone();
    assert_eq!(payloads.len(), 1);
    assert!(
        payloads
            .first()
            .is_some_and(|payload| payload.get("removed").is_none())
    );
    fixture.runtime.shutdown().await?;
    Ok(())
}
