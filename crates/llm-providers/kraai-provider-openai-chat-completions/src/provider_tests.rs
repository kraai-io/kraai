#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "provider tests use direct assertions for local HTTP fixtures"
)]

use super::*;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use kraai_provider_core::{ProviderRequestContext, ProviderRetryEvent, ProviderRetryObserver};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn provider_creation_uses_the_same_catalog_identity_as_pricing() {
    let credentials = DynamicConfig::from([("api_key".into(), DynamicValue::from("fixture"))]);
    let direct = create_provider::<OpenAiChatCompletionsProfile>(
        ProviderId::new("direct"),
        credentials.clone(),
    )
    .unwrap();
    assert_eq!(direct.catalog_provider.as_deref(), Some("openai"));
    for (endpoint, override_id, expected) in [
        ("https://api.openai.com/v1/", None, Some("openai")),
        ("https://api.openai.com/v1", Some("custom"), Some("custom")),
        ("https://proxy.test/v1", Some("custom"), Some("custom")),
        ("https://api.groq.com/openai/v1", None, None),
    ] {
        let mut config = credentials.clone();
        config.insert("base_url".into(), DynamicValue::from(endpoint));
        if let Some(id) = override_id {
            config.insert("catalog_provider".into(), DynamicValue::from(id));
        }
        let pricing = GenericChatCompletionsProfile::pricing_catalog(&config);
        let provider =
            create_provider::<GenericChatCompletionsProfile>(ProviderId::new("compatible"), config)
                .unwrap();
        assert_eq!(provider.catalog_provider.as_deref(), expected);
        assert_eq!(provider.catalog_provider, pricing.provider);
    }
}

fn is_missing_system_ca_error(error: &dyn std::error::Error) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        let display = error.to_string();
        let debug = format!("{error:?}");
        if display.contains("No CA certificates were loaded from the system")
            || debug.contains("No CA certificates were loaded from the system")
            || display == "builder error"
        {
            return true;
        }
        current = error.source();
    }
    false
}

fn test_client_or_skip() -> Option<Client> {
    match Client::builder().timeout(Duration::from_secs(2)).build() {
        Ok(client) => Some(client),
        Err(error) if is_missing_system_ca_error(&error) => None,
        Err(error) => panic!("unexpected reqwest client build error: {error}"),
    }
}

fn discovery_provider(
    address: SocketAddr,
) -> Option<ChatCompletionsProvider<GenericChatCompletionsProfile>> {
    match create_provider::<GenericChatCompletionsProfile>(
        ProviderId::new("fixture"),
        DynamicConfig::from([
            (
                "base_url".into(),
                DynamicValue::String(format!("http://{address}")),
            ),
            ("api_key".into(), DynamicValue::String("fixture-key".into())),
            ("only_listed_models".into(), DynamicValue::Bool(false)),
        ]),
    ) {
        Ok(provider) => Some(provider),
        Err(error) if is_missing_system_ca_error(error.as_ref()) => None,
        Err(error) => panic!("unexpected provider creation error: {error}"),
    }
}

#[derive(Clone, Default)]
struct RetryCollector {
    events: Arc<Mutex<Vec<ProviderRetryEvent>>>,
}

impl RetryCollector {
    fn snapshot(&self) -> Vec<ProviderRetryEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl ProviderRetryObserver for RetryCollector {
    fn on_retry_scheduled(&self, event: &ProviderRetryEvent) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event.clone());
    }
}

enum ScriptedResponse {
    Status {
        status_line: &'static str,
        body: &'static str,
    },
}

async fn spawn_server(script: Vec<ScriptedResponse>) -> SocketAddr {
    spawn_recording_server(script).await.0
}

async fn spawn_recording_server(
    script: Vec<ScriptedResponse>,
) -> (SocketAddr, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for next in script {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        .unwrap_or_default();
                    if bytes.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());

            match next {
                ScriptedResponse::Status { status_line, body } => {
                    write_json_response(&mut stream, status_line, body).await;
                }
            }
        }
        requests
    });
    (address, task)
}

async fn write_json_response(stream: &mut tokio::net::TcpStream, status_line: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    stream.write_all(response.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
}

#[tokio::test]
async fn generate_reply_stream_forwards_retry_observer_to_http_retry_layer() {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "429 Too Many Requests",
            body: r#"{"error":{"message":"slow down"}}"#,
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            body: "data: {\"choices\":[{\"delta\":{\"content\":\"ok after retry\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        },
    ])
    .await;

    let Some(client) = test_client_or_skip() else {
        return;
    };

    let provider = ChatCompletionsProvider::<GenericChatCompletionsProfile> {
        id: ProviderId::new("openai-chat-completions"),
        client,
        base_url: format!("http://{address}"),
        auth: ApiKeyAuth::resolve(&BTreeMap::from([(
            String::from("api_key"),
            kraai_provider_core::DynamicValue::from("test-key"),
        )]))
        .unwrap(),
        only_listed_models: false,
        cached_models: ModelMetadataCache::default(),
        model_configs: BTreeMap::new(),
        model_catalog: None,
        catalog_provider: None,
        _profile: PhantomData,
    };

    let collector = Arc::new(RetryCollector::default());
    let events = provider
        .generate_reply_stream(
            &ModelId::new("gpt-4.1-mini"),
            ProviderRequest {
                cacheable_messages: None,
                options: Default::default(),
                messages: vec![kraai_types::ConversationItem::User {
                    content: String::from("hello").into(),
                }],
                script_tool: None,
            },
            &ProviderRequestContext::with_retry_observer(collector.clone()),
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        events.first(),
        Some(Ok(ProviderStreamEvent::TextDelta { delta, .. }))
            if delta == "ok after retry"
    ));

    let retries = collector.snapshot();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].operation, "chat completions stream");
    assert_eq!(retries[0].retry_number, 1);
    assert_eq!(retries[0].reason, "HTTP 429 Too Many Requests");
}

#[tokio::test]
async fn successful_http_status_does_not_hide_a_streamed_provider_failure() {
    let address = spawn_server(vec![ScriptedResponse::Status {
        status_line: "200 OK",
        body: concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"error\":{\"code\":502,\"message\":\"Provider disconnected\"},\"choices\":[{\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n",
            "data: [DONE]\n\n",
        ),
    }])
    .await;
    let provider = ChatCompletionsProvider::<GenericChatCompletionsProfile> {
        id: ProviderId::new("fixture"),
        client: Client::builder().tls_certs_only([]).build().unwrap(),
        base_url: format!("http://{address}"),
        auth: ApiKeyAuth::resolve(&BTreeMap::from([(
            String::from("api_key"),
            DynamicValue::from("test-key"),
        )]))
        .unwrap(),
        only_listed_models: false,
        cached_models: ModelMetadataCache::default(),
        model_configs: BTreeMap::new(),
        model_catalog: None,
        catalog_provider: None,
        _profile: PhantomData,
    };
    let events = provider
        .generate_reply_stream(
            &ModelId::new("fixture-model"),
            ProviderRequest {
                cacheable_messages: None,
                options: Default::default(),
                messages: vec![kraai_types::ConversationItem::User {
                    content: String::from("hello").into(),
                }],
                script_tool: None,
            },
            &ProviderRequestContext::default(),
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(
        events.first(),
        Some(Ok(ProviderStreamEvent::TextDelta { delta, .. })) if delta == "partial"
    ));
    assert!(events.get(1).is_some_and(|event| {
        event
            .as_ref()
            .is_err_and(|error| error.to_string().contains("Provider disconnected"))
    }));
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn model_cache_replacement_preserves_selection_metadata_and_failed_refreshes() {
    for only_listed_models in [false, true] {
        let address = spawn_server(vec![
            ScriptedResponse::Status {
                status_line: "200 OK",
                body: r#"{"data":[{"id":"zeta"},{"id":"alpha"},{"id":"alpha"}]}"#,
            },
            ScriptedResponse::Status {
                status_line: "200 OK",
                body: r#"{"data":[{"id":42}]}"#,
            },
        ])
        .await;
        let Some(client) = test_client_or_skip() else {
            return;
        };
        let cached_models = ModelMetadataCache::default();
        cached_models
            .refresh(None, async { Ok(Vec::new()) }, |_, _| {
                BTreeMap::from([(
                    ModelId::new("stale"),
                    Model {
                        supports_images: false,
                        id: ModelId::new("stale"),
                        name: String::from("Stale model"),
                        max_context: None,
                        options: Vec::new(),
                    },
                )])
            })
            .await
            .unwrap();
        let provider = ChatCompletionsProvider::<GenericChatCompletionsProfile> {
            id: ProviderId::new("fixture"),
            client,
            base_url: format!("http://{address}"),
            auth: ApiKeyAuth::resolve(&BTreeMap::from([(
                String::from("api_key"),
                DynamicValue::from("fixture-key"),
            )]))
            .unwrap(),
            only_listed_models,
            cached_models,
            model_configs: BTreeMap::from([(
                ModelId::new("alpha"),
                ConfiguredModelMetadata {
                    supports_images: None,
                    name: Some(String::from("Configured alpha")),
                    max_context: Some(4096),
                    options: Vec::new(),
                    remove_options: Vec::new(),
                },
            )]),
            model_catalog: None,
            catalog_provider: None,
            _profile: PhantomData,
        };
        let metadata = |models: Vec<Model>| {
            models
                .into_iter()
                .map(|model| (model.id.to_string(), model.name, model.max_context))
                .collect::<Vec<_>>()
        };
        let mut expected = vec![(
            String::from("alpha"),
            String::from("Configured alpha"),
            Some(4096),
        )];
        if !only_listed_models {
            expected.push((String::from("zeta"), String::from("zeta"), None));
        }

        provider.cache_models().await.unwrap();
        assert_eq!(metadata(provider.list_models().await), expected);
        for listed in provider.list_models().await {
            let found = provider.get_model(&listed.id).await.unwrap();
            assert_eq!(metadata(vec![found]), metadata(vec![listed]));
        }
        assert!(provider.get_model(&ModelId::new("unknown")).await.is_none());
        assert!(provider.get_model(&ModelId::new("stale")).await.is_none());
        if only_listed_models {
            assert!(provider.get_model(&ModelId::new("zeta")).await.is_none());
        }
        assert!(provider.cache_models().await.is_err());
        assert_eq!(metadata(provider.list_models().await), expected);
    }
}

#[tokio::test]
async fn discovery_requires_advertised_reasoning_and_applies_native_modes_to_http_requests() {
    let (address, server) = spawn_recording_server(vec![
        ScriptedResponse::Status { status_line: "200 OK", body: r#"{"data":[{"id":"future-model","reasoning_options":[{"type":"effort","values":[null,"future-effort"]}],"experimental":{"modes":{"fast":{"provider":{"body":{"speed":"fast"},"headers":{"x-mode":"fast"}}}}}}]}"# },
        ScriptedResponse::Status { status_line: "200 OK", body: "data: [DONE]\n\n" },
    ]).await;
    let Some(provider) = discovery_provider(address) else {
        return;
    };
    provider.cache_models().await.unwrap();
    let id = ModelId::new("future-model");
    let make_request = |options| ProviderRequest {
        messages: Vec::new(),
        script_tool: None,
        cacheable_messages: None,
        options,
    };
    assert!(
        provider
            .generate_reply_stream(
                &id,
                make_request(Default::default()),
                &ProviderRequestContext::default()
            )
            .await
            .is_err()
    );
    let options = kraai_types::ModelOptionValues::from([
        (
            "reasoning_effort".into(),
            kraai_types::ModelOptionValue::Choice("future-effort".into()),
        ),
        (
            "mode:fast".into(),
            kraai_types::ModelOptionValue::Boolean(true),
        ),
    ]);
    provider
        .generate_reply_stream(
            &id,
            make_request(options),
            &ProviderRequestContext::default(),
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    let (headers, body) = requests[1].split_once("\r\n\r\n").unwrap();
    assert!(headers.to_ascii_lowercase().contains("x-mode: fast"));
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body.get("model"), Some(&serde_json::json!("future-model")));
    assert_eq!(
        body.get("reasoning_effort"),
        Some(&serde_json::json!("future-effort"))
    );
    assert_eq!(body.get("speed"), Some(&serde_json::json!("fast")));
}

#[tokio::test]
async fn supported_reasoning_levels_preserve_explicit_descriptors_and_generic_toggle_conditions() {
    let mut responses = vec![ScriptedResponse::Status {
        status_line: "200 OK",
        body: r#"{"data":[
            {"id":"explicit-model","supported_reasoning_levels":[{"effort":"fallback-effort"}],"options":[
                {"id":"reasoning_enabled","label":"Thinking","type":"boolean","required":true,"binding":{"type":"body","path":"/thinking/enabled"}},
                {"id":"reasoning_effort","label":"Endpoint effort","type":"choice","required":true,
                    "active_when":{"option":"reasoning_enabled","value":true},"binding":{"type":"body","path":"/custom/effort"},
                    "choices":[{"id":"endpoint-effort","label":"Endpoint effort"}]}
            ]},
            {"id":"fallback-model","supported_reasoning_levels":[{"effort":"fallback-effort"}],"options":[
                {"id":"reasoning_enabled","label":"Thinking","type":"boolean","required":true,"binding":{"type":"body","path":"/thinking/enabled"}}
            ]}
        ]}"#,
    }];
    responses.extend((0..4).map(|_| ScriptedResponse::Status {
        status_line: "200 OK",
        body: "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    }));
    let (address, server) = spawn_recording_server(responses).await;
    let Some(provider) = discovery_provider(address) else {
        return;
    };
    provider.cache_models().await.unwrap();
    let make_request = |options| ProviderRequest {
        messages: Vec::new(),
        script_tool: None,
        cacheable_messages: None,
        options,
    };
    for (model, effort) in [
        ("explicit-model", "endpoint-effort"),
        ("fallback-model", "fallback-effort"),
    ] {
        let id = ModelId::new(model);
        for enabled in [false, true] {
            let mut options = kraai_types::ModelOptionValues::from([(
                "reasoning_enabled".into(),
                kraai_types::ModelOptionValue::Boolean(enabled),
            )]);
            if enabled {
                assert!(
                    provider
                        .generate_reply_stream(
                            &id,
                            make_request(options.clone()),
                            &ProviderRequestContext::default(),
                        )
                        .await
                        .is_err()
                );
                options.insert(
                    "reasoning_effort".into(),
                    kraai_types::ModelOptionValue::Choice(effort.into()),
                );
            }
            provider
                .generate_reply_stream(
                    &id,
                    make_request(options),
                    &ProviderRequestContext::default(),
                )
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap();
        }
    }
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 5);
    for (request, (model, path, effort)) in requests.iter().skip(1).zip([
        ("explicit-model", "/custom/effort", None),
        ("explicit-model", "/custom/effort", Some("endpoint-effort")),
        ("fallback-model", "/reasoning_effort", None),
        (
            "fallback-model",
            "/reasoning_effort",
            Some("fallback-effort"),
        ),
    ]) {
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body.get("model"), Some(&serde_json::json!(model)));
        assert_eq!(
            body.pointer("/thinking/enabled"),
            Some(&serde_json::json!(effort.is_some()))
        );
        assert_eq!(
            body.pointer(path),
            effort.map(|effort| serde_json::json!(effort)).as_ref()
        );
        if model == "explicit-model" {
            assert!(body.get("reasoning_effort").is_none());
        }
    }
}

#[tokio::test]
async fn configured_reasoning_options_keep_unconditional_and_custom_conditions_after_discovery() {
    let (address, server) = spawn_recording_server(vec![
        ScriptedResponse::Status {
            status_line: "200 OK",
            body: r#"{"data":[{"id":"configured-model","supported_reasoning_levels":[{"effort":"native-effort"}],"options":[
                {"id":"reasoning_enabled","label":"Thinking","type":"boolean","required":true,"binding":{"type":"body","path":"/thinking/enabled"}}
            ]}]}"#,
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            body: "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            body: "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        },
    ])
    .await;
    let Some(mut provider) = discovery_provider(address) else {
        return;
    };
    let id = ModelId::new("configured-model");
    let options = serde_json::from_value(serde_json::json!([
        {"id":"custom_gate","label":"Custom gate","type":"boolean","required":true,
            "binding":{"type":"body","path":"/custom/enabled"}},
        {"id":"reasoning_effort","label":"Configured effort","type":"choice","required":true,
            "binding":{"type":"body","path":"/custom/effort"},
            "choices":[{"id":"configured-effort","label":"Configured effort"}]},
        {"id":"reasoning_budget","label":"Configured budget","type":"integer","required":true,
            "min":1,"max":10,"binding":{"type":"body","path":"/custom/budget"},
            "active_when":{"option":"custom_gate","value":true}}
    ]))
    .unwrap();
    provider
        .register_model(ModelConfig {
            id: id.clone(),
            provider_id: provider.id.clone(),
            config: DynamicConfig::new(),
            options,
            remove_options: Vec::new(),
        })
        .await
        .unwrap();
    provider.cache_models().await.unwrap();
    let make_request = |options| ProviderRequest {
        messages: Vec::new(),
        script_tool: None,
        cacheable_messages: None,
        options,
    };
    for enabled in [true, false] {
        let mut selected = kraai_types::ModelOptionValues::from([
            (
                "reasoning_enabled".into(),
                kraai_types::ModelOptionValue::Boolean(false),
            ),
            (
                "custom_gate".into(),
                kraai_types::ModelOptionValue::Boolean(enabled),
            ),
        ]);
        if enabled {
            selected.insert(
                "reasoning_budget".into(),
                kraai_types::ModelOptionValue::Integer(7),
            );
        }
        assert!(
            provider
                .generate_reply_stream(
                    &id,
                    make_request(selected.clone()),
                    &ProviderRequestContext::default()
                )
                .await
                .is_err()
        );
        selected.insert(
            "reasoning_effort".into(),
            kraai_types::ModelOptionValue::Choice("configured-effort".into()),
        );
        provider
            .generate_reply_stream(
                &id,
                make_request(selected),
                &ProviderRequestContext::default(),
            )
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
    }
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 3);
    for (request, enabled) in requests.iter().skip(1).zip([true, false]) {
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            body.pointer("/thinking/enabled"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            body.pointer("/custom/enabled"),
            Some(&serde_json::json!(enabled))
        );
        assert_eq!(
            body.pointer("/custom/effort"),
            Some(&serde_json::json!("configured-effort"))
        );
        assert_eq!(
            body.pointer("/custom/budget"),
            enabled.then_some(&serde_json::json!(7))
        );
        assert!(body.get("reasoning_effort").is_none());
    }
}

#[tokio::test]
async fn configured_custom_models_absent_from_discovery_apply_custom_settings() {
    let (address, server) = spawn_recording_server(vec![
        ScriptedResponse::Status {
            status_line: "200 OK",
            body: r#"{"data":[]}"#,
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            body: "data: [DONE]\n\n",
        },
    ])
    .await;
    let Some(mut provider) = discovery_provider(address) else {
        return;
    };
    let id = ModelId::new("custom-model");
    let definition = serde_json::from_value(serde_json::json!({
        "id":"custom-budget","label":"Custom Budget","required":true,"type":"integer","min":1,"max":10000,
        "binding":{"type":"body","path":"/custom/thinking_budget"}
    })).unwrap();
    provider
        .register_model(ModelConfig {
            id: id.clone(),
            provider_id: provider.id.clone(),
            config: DynamicConfig::new(),
            options: vec![definition],
            remove_options: Vec::new(),
        })
        .await
        .unwrap();
    provider.cache_models().await.unwrap();
    assert_eq!(provider.list_models().await.len(), 1);
    assert_eq!(provider.get_model(&id).await.unwrap().options.len(), 1);
    let request = ProviderRequest {
        messages: Vec::new(),
        script_tool: None,
        cacheable_messages: None,
        options: kraai_types::ModelOptionValues::from([(
            "custom-budget".into(),
            kraai_types::ModelOptionValue::Integer(4000),
        )]),
    };
    provider
        .generate_reply_stream(&id, request, &ProviderRequestContext::default())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let requests = server.await.unwrap();
    let body: serde_json::Value =
        serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        body.pointer("/custom/thinking_budget"),
        Some(&serde_json::json!(4000))
    );
    assert_eq!(body.get("model"), Some(&serde_json::json!("custom-model")));
}
