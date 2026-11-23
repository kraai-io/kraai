use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};
use kraai_runtime::{Event, RuntimeBuilder, RuntimeStartupState};
use kraai_types::{
    ModelId, ModelOptionDefinition, ModelOptionValue, ModelOptionValues, ModelSelection, ProviderId,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::task::AbortOnDropHandle;

use super::*;

struct Fixture {
    runtime: RuntimeHandle,
    session: SessionId,
    payloads: Arc<Mutex<Vec<Value>>>,
    _server: AbortOnDropHandle<()>,
    _root: tempfile::TempDir,
}

impl Fixture {
    async fn new(definitions: Vec<ModelOptionDefinition>, values: ModelOptionValues) -> Self {
        let root = tempfile::tempdir().unwrap();
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
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
        .await
        .unwrap();
        let runtime = RuntimeBuilder::new(kraai_providers::DefaultProviders)
            .storage_root(root.path().join("state"))
            .provider_config_path(config)
            .mcp_config_path(root.path().join("mcp.toml"))
            .build_on(&tokio::runtime::Handle::current());
        assert_eq!(
            runtime.wait_for_startup().await.unwrap(),
            RuntimeStartupState::ready()
        );
        let session = runtime.create_session().await.unwrap();
        let fixture = Self {
            runtime,
            session,
            payloads,
            _server: server,
            _root: root,
        };
        fixture.update(definitions).await;
        fixture
            .runtime
            .set_session_model(fixture.session.clone(), selection(values))
            .await
            .unwrap();
        fixture
    }

    async fn update(&self, definitions: Vec<ModelOptionDefinition>) {
        let mut settings = self.runtime.get_settings().await.unwrap();
        settings
            .models
            .first_mut()
            .expect("missing configured model")
            .options = definitions;
        self.runtime.save_settings(settings).await.unwrap();
    }
}

fn selection(options: ModelOptionValues) -> ModelSelection {
    ModelSelection {
        provider_id: ProviderId::new("mock"),
        model_id: ModelId::new("model"),
        options,
    }
}

fn changing_definitions(effort: &str, maximum: i64) -> Vec<ModelOptionDefinition> {
    serde_json::from_value(json!([
        {"id":"effort","label":"Effort","required":true,"type":"choice",
            "choices":[{"id":effort,"label":effort,"patch":{"body":{"reasoning_effort":effort}}}]},
        {"id":"budget","label":"Budget","type":"integer","min":0,"max":maximum,
            "binding":{"type":"body","path":"/budget"}},
        {"id":"retained","label":"Retained","type":"boolean","binding":{"type":"body","path":"/retained"}}
    ])).unwrap()
}

#[tokio::test]
async fn active_session_metadata_changes_reconcile_controls_and_allow_individual_edits() {
    let old_values = ModelOptionValues::from([
        ("effort".into(), ModelOptionValue::Choice("high".into())),
        ("budget".into(), ModelOptionValue::Integer(80)),
        ("retained".into(), ModelOptionValue::Boolean(false)),
    ]);
    let fixture = Fixture::new(changing_definitions("high", 100), old_values.clone()).await;
    let reselected = fixture.runtime.create_session().await.unwrap();
    fixture
        .runtime
        .set_session_model(reselected.clone(), selection(old_values.clone()))
        .await
        .unwrap();
    fixture.update(changing_definitions("low", 50)).await;
    let descriptors = options(&fixture.runtime, &fixture.session).await.unwrap();
    for id in ["option:effort", "option:budget"] {
        let descriptor = descriptors
            .iter()
            .find(|option| option.id.0.as_ref() == id)
            .unwrap_or_else(|| panic!("missing option {id}"));
        assert_eq!(
            serde_json::to_value(descriptor)
                .unwrap()
                .get("currentValue"),
            Some(&json!(""))
        );
    }
    let selected = crate::session::selected_model(&fixture.runtime, &fixture.session)
        .await
        .unwrap();
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
            .await
            .unwrap()
            .map(|model| model.options),
        Some(old_values)
    );
    set_model_option(&fixture.runtime, &fixture.session, "effort", "low")
        .await
        .unwrap();
    set_model_option(&fixture.runtime, &fixture.session, "budget", "40")
        .await
        .unwrap();
    assert_eq!(
        fixture
            .runtime
            .get_session_model(fixture.session.clone())
            .await
            .unwrap()
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
    .await
    .unwrap();
    assert_eq!(
        fixture
            .runtime
            .get_session_model(reselected)
            .await
            .unwrap()
            .map(|model| model.options),
        Some(ModelOptionValues::from([(
            "retained".into(),
            ModelOptionValue::Boolean(false)
        )]))
    );
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn removing_the_final_optional_setting_does_not_block_the_next_prompt() {
    let definitions = serde_json::from_value(json!([{
        "id":"removed","label":"Removed","type":"boolean","binding":{"type":"body","path":"/removed"}
    }])).unwrap();
    let fixture = Fixture::new(
        definitions,
        ModelOptionValues::from([("removed".into(), ModelOptionValue::Boolean(true))]),
    )
    .await;
    fixture.update(Vec::new()).await;
    let selected = crate::session::selected_model(&fixture.runtime, &fixture.session)
        .await
        .unwrap();
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
        .await
        .unwrap();
    kraai_test_support::hang_guard(async {
        loop {
            let event = events.recv().await.unwrap().event;
            if let Event::StreamError { error, .. } = event {
                panic!("{error}");
            }
            if matches!(event, Event::TurnCompleted { .. }) {
                return;
            }
        }
    })
    .await
    .unwrap();
    let payloads = fixture.payloads.lock().await.clone();
    assert_eq!(payloads.len(), 1);
    assert!(
        payloads
            .first()
            .is_some_and(|payload| payload.get("removed").is_none())
    );
    fixture.runtime.shutdown().await.unwrap();
}
