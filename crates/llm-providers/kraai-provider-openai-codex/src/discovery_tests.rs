#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert HTTP behavior and propagate transport errors"
)]

use super::*;
use crate::auth::OpenAiCodexAuthControllerOptions;
use color_eyre::eyre::ensure;
use futures::TryStreamExt;
use kraai_provider_core::ScriptToolDefinition;
use kraai_types::{AssistantPhase, ConversationItem, TokenUsage};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

async fn server(
    responses: Vec<(&'static str, String)>,
) -> Result<(String, JoinHandle<Result<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}/backend-api", listener.local_addr()?);
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(10), async move {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await?;
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0_u8; 4096];
                    let read = stream.read(&mut buffer).await?;
                    ensure!(read > 0, "request ended early");
                    bytes.extend_from_slice(buffer.get(..read).ok_or_else(|| eyre!("invalid read length"))?);
                    if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(bytes.get(..header_end).ok_or_else(|| eyre!("missing headers"))?);
                        let content_length = headers.lines().filter_map(|line| line.split_once(':'))
                            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, value)| value.trim().parse::<usize>()).transpose()?.unwrap_or_default();
                        if bytes.len() >= header_end + 4 + content_length { break; }
                    }
                }
                requests.push(String::from_utf8(bytes)?);
                let content_type = if body.starts_with("data:") { "text/event-stream" } else { "application/json" };
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).await?;
            }
            Ok::<_, color_eyre::Report>(requests)
        }).await?
    });
    Ok((base_url, task))
}

fn provider(base_url: String) -> Result<OpenAiCodexProvider> {
    Ok(OpenAiCodexProvider {
        id: ProviderId::new("test-codex"),
        auth: Arc::new(OpenAiCodexAuthController::new_with_options(
            OpenAiCodexAuthControllerOptions::new(
                std::env::temp_dir()
                    .join(format!("codex-discovery-{}", ulid::Ulid::generate()))
                    .join("auth.json"),
            ),
        )?),
        client: build_codex_http_client(true, false, &base_url)?,
        models: RwLock::new(DiscoveredModels::default()),
        rejected_reasoning: RwLock::new(RejectedReasoning::default()),
        model_configs: BTreeMap::new(),
        base_url,
        proxy_token: Some("test-token".into()),
        allow_http_proxy: false,
    })
}

#[test]
fn subscription_config_rejects_http_backends() {
    for url in [
        "http://example.com/backend-api",
        "http://127.0.0.1/backend-api",
    ] {
        let config = DynamicConfig::from([("base_url".into(), DynamicValue::String(url.into()))]);
        assert!(!OpenAiCodexFactory::validate_provider_config(&config).is_empty());
    }
}

#[test]
fn backend_transport_validation_allows_https_and_only_loopback_http_proxies() {
    for (url, subscription_allowed, proxy_allowed) in [
        ("https://chatgpt.com/backend-api", true, true),
        ("https://proxy.example.com/backend-api", true, true),
        ("http://127.0.0.1:1234/backend-api", false, true),
        ("http://127.0.0.2/backend-api", false, true),
        ("http://[::1]/backend-api", false, true),
        ("http://localhost/backend-api", false, true),
        ("http://0.0.0.0/backend-api", false, false),
        ("http://192.168.1.10/backend-api", false, false),
        ("http://[::]/backend-api", false, false),
        ("http://example.com/backend-api", false, false),
        ("http://localhost.example.com/backend-api", false, false),
        ("http://127.0.0.1.example.com/backend-api", false, false),
    ] {
        for (proxy_token, allowed) in [(false, subscription_allowed), (true, proxy_allowed)] {
            assert_eq!(valid_backend_url(url, proxy_token, false), allowed, "{url}");
            let mut config =
                DynamicConfig::from([("base_url".into(), DynamicValue::String(url.into()))]);
            if proxy_token {
                config.insert(
                    "proxy_token_env".into(),
                    DynamicValue::String("TEST_PROXY_TOKEN".into()),
                );
            }
            assert_eq!(
                OpenAiCodexFactory::validate_provider_config(&config).is_empty(),
                allowed,
                "{url}"
            );
        }
    }
}

#[tokio::test]
async fn unsafe_backends_are_rejected_before_loading_auth_or_building_requests() -> Result<()> {
    for (url, proxy_token) in [
        ("http://example.com/backend-api", None),
        ("http://127.0.0.1/backend-api", None),
        ("http://example.com/backend-api", Some("test-token".into())),
    ] {
        let mut provider = provider(url.into())?;
        provider.proxy_token = proxy_token;
        for operation in ["list models", "responses"] {
            let built = std::sync::atomic::AtomicBool::new(false);
            let result = provider
                .send_authenticated_request(operation, &ProviderRequestContext::default(), |auth| {
                    built.store(true, std::sync::atomic::Ordering::Relaxed);
                    provider.authenticated_get(url, auth)
                })
                .await;
            assert!(result.is_err_and(|error| error.to_string() == BACKEND_URL_ERROR));
            assert!(!built.load(std::sync::atomic::Ordering::Relaxed));
        }
        let factory = OpenAiCodexFactory::new(provider.auth.clone());
        if provider.proxy_token.is_none() {
            let config =
                DynamicConfig::from([("base_url".into(), DynamicValue::String(url.into()))]);
            assert!(
                factory
                    .create(ProviderId::new("unsafe"), config)
                    .is_err_and(|error| error.to_string() == BACKEND_URL_ERROR)
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn tool_free_summaries_stream_text_and_usage_through_authenticated_requests() -> Result<()> {
    let models = json!({"models": [{
        "slug": "summary-model",
        "display_name": "Summary Model",
        "visibility": "list",
        "default_reasoning_level": "low",
        "supported_reasoning_levels": [{"effort": "low", "description": "Fast"}]
    }]})
    .to_string();
    let summary = "Routing fixed. Validate the remaining changes.";
    let events = [
        json!({"type": "response.output_text.delta", "item_id": "summary", "delta": summary}),
        json!({"type": "response.completed", "response": {"usage": {
            "input_tokens": 100, "output_tokens": 20,
            "input_tokens_details": {"cached_tokens": 40},
            "output_tokens_details": {"reasoning_tokens": 5}
        }}}),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect::<String>();
    let (base_url, server) = server(vec![
        ("200 OK", models),
        ("200 OK", events.clone()),
        ("200 OK", events),
    ])
    .await?;
    let provider = provider(base_url)?;
    provider.cache_models().await?;
    for cache_key in [None, Some("summary-session")] {
        let context = cache_key.map_or_else(ProviderRequestContext::default, |key| {
            ProviderRequestContext::with_prompt_cache_key(key.into())
        });
        let events = provider
            .generate_reply_stream(
                &ModelId::new("summary-model-low"),
                ProviderRequest {
                    cacheable_messages: None,
                    messages: vec![
                        ConversationItem::System {
                            text: "Summarize the conversation. Do not execute tools.".into(),
                        },
                        ConversationItem::User {
                            text:
                                "Previous summary:\n\nAdditional conversation data:\nRouting fixed."
                                    .into(),
                        },
                    ],
                    script_tool: None,
                },
                &context,
            )
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        assert_eq!(
            events,
            vec![
                ProviderStreamEvent::TextDelta {
                    item_id: "summary".into(),
                    phase: AssistantPhase::FinalAnswer,
                    delta: summary.into(),
                },
                ProviderStreamEvent::Usage(TokenUsage {
                    total_tokens: 120,
                    input_tokens: 60,
                    output_tokens: 15,
                    reasoning_tokens: 5,
                    cache_read_tokens: 40,
                    ..TokenUsage::default()
                }),
            ]
        );
    }
    let requests = server.await??;
    assert_eq!(requests.len(), 3);
    for (request, cache_key) in requests.iter().skip(1).zip([None, Some("summary-session")]) {
        assert!(request.starts_with("POST /backend-api/codex/responses HTTP/1.1\r\n"));
        let (headers, body) = request
            .split_once("\r\n\r\n")
            .ok_or_else(|| eyre!("missing request body"))?;
        let headers = headers.to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer test-token\r\n"));
        assert!(headers.contains("accept: text/event-stream"));
        assert_eq!(
            headers
                .lines()
                .find_map(|line| line.strip_prefix("session_id: ")),
            cache_key
        );
        let body: Value = serde_json::from_str(body)?;
        assert_eq!(body.get("model"), Some(&json!("summary-model")));
        assert_eq!(
            body.get("instructions"),
            Some(&json!("Summarize the conversation. Do not execute tools."))
        );
        assert_eq!(
            body.get("input"),
            Some(&json!([{
                "type": "message", "role": "user", "content": [{
                    "type": "input_text", "text": "Previous summary:\n\nAdditional conversation data:\nRouting fixed."
                }]
            }]))
        );
        assert_eq!(body.get("tools"), Some(&json!([])));
        assert_eq!(body.get("tool_choice"), Some(&json!("none")));
        assert!(body.get("parallel_tool_calls").is_none());
        assert_eq!(body.get("stream"), Some(&json!(true)));
        assert_eq!(body.get("store"), Some(&json!(false)));
        assert_eq!(
            body.get("prompt_cache_key").and_then(Value::as_str),
            cache_key
        );
    }
    Ok(())
}

async fn redirect_server(location: &str) -> Result<(String, JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = [0_u8; 4096];
        ensure!(
            stream.read(&mut buffer).await? > 0,
            "missing redirect request"
        );
        stream.write_all(response.as_bytes()).await?;
        Ok::<_, color_eyre::Report>(())
    });
    Ok((format!("http://{address}/backend-api"), server))
}

#[tokio::test]
async fn subscription_redirect_policy_rejects_cross_origin_https_targets() -> Result<()> {
    let origin = Url::parse("https://backend.example/backend-api")?.origin();
    for target in [
        "https://elsewhere.example/codex/models",
        "https://backend.example:8443/codex/models",
    ] {
        let (base_url, server) = redirect_server(target).await?;
        let client = streaming_http_client_builder()
            .redirect(codex_redirect_policy(false, false, origin.clone()))
            .build()?;
        let result = client
            .get(base_url)
            .header("ChatGPT-Account-Id", "test-account")
            .send()
            .await;
        assert!(result.is_err_and(|error| error.is_redirect()), "{target}");
        server.await??;
    }
    Ok(())
}

#[tokio::test]
async fn proxy_redirect_cannot_bypass_backend_transport_validation() -> Result<()> {
    let (base_url, server) = redirect_server("http://example.com/backend-api/codex/models").await?;
    let provider = provider(base_url)?;
    let result = provider.cache_models().await;
    assert!(result.is_err_and(|error| {
        error
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_redirect)
    }));
    server.await??;
    Ok(())
}

#[tokio::test]
async fn subscription_client_rejects_http_requests() -> Result<()> {
    let result = build_codex_http_client(false, false, DEFAULT_CHATGPT_BACKEND_URL)?
        .get("http://127.0.0.1:1/backend-api/codex/models")
        .send()
        .await;
    assert!(result.is_err_and(|error| error.is_builder()));
    Ok(())
}

#[tokio::test]
async fn discovery_drives_requests_and_reports_refresh_failures() -> Result<()> {
    let body = json!({"models": [{
        "slug": "brand-new-codex",
        "display_name": "Brand New",
        "visibility": "list",
        "context_window": 234567,
        "default_reasoning_level": "high",
        "supported_reasoning_levels": [{"effort": "high", "description": "Thorough"}]
    }]})
    .to_string();
    let (base_url, server) = server(vec![
        ("200 OK", body),
        ("200 OK", "{}".into()),
        ("200 OK", r#"{"data":[]}"#.into()),
        ("403 Forbidden", "denied".into()),
        ("200 OK", r#"{"models":[]}"#.into()),
    ])
    .await?;
    let provider = provider(base_url)?;
    provider.cache_models().await?;
    let models = provider.list_models().await;
    assert_eq!(models.len(), 1);
    let model = models.first().ok_or_else(|| eyre!("no discovered model"))?;
    assert_eq!(model.id.as_str(), "brand-new-codex-high");
    assert!(provider.cache_warming_policy(&model.id).is_some());
    assert_eq!(model.name, "Brand New high");
    assert_eq!(model.max_context, Some(234567));
    let request = ProviderRequest {
        cacheable_messages: None,
        messages: vec![],
        script_tool: Some(ScriptToolDefinition {
            name: "kraai_nushell".into(),
            description: "Run a script".into(),
        }),
    };
    provider
        .send_responses_request(
            &model.id,
            request,
            &ProviderRequestContext::default(),
            false,
        )
        .await?
        .text()
        .await?;
    assert!(provider.cache_models().await.is_err());
    assert!(
        provider
            .cache_models()
            .await
            .is_err_and(|error| error.to_string().contains("403"))
    );
    provider.cache_models().await?;
    assert!(provider.list_models().await.is_empty());
    assert!(provider.models.read().await.resolve(&model.id).is_err());

    let requests = server.await??;
    assert_eq!(requests.len(), 5);
    for request in requests.iter().filter(|request| request.starts_with("GET")) {
        assert!(
            request
                .starts_with("GET /backend-api/codex/models?client_version=0.154.0 HTTP/1.1\r\n")
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-token")
        );
    }
    let request = requests
        .iter()
        .find(|request| request.starts_with("POST /backend-api/codex/responses "))
        .ok_or_else(|| eyre!("no responses request"))?;
    let (_, body) = request
        .split_once("\r\n\r\n")
        .ok_or_else(|| eyre!("missing request body"))?;
    let body: Value = serde_json::from_str(body)?;
    assert_eq!(body.get("model"), Some(&json!("brand-new-codex")));
    assert_eq!(
        body.get("reasoning")
            .and_then(|reasoning| reasoning.get("effort")),
        Some(&json!("high"))
    );
    assert!(
        body.get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| !tools.is_empty())
    );
    assert_eq!(body.get("tool_choice"), Some(&json!("auto")));
    assert_eq!(body.get("parallel_tool_calls"), Some(&json!(false)));
    Ok(())
}

#[tokio::test]
async fn initial_discovery_failure_does_not_create_models() -> Result<()> {
    let (base_url, server) = server(vec![("403 Forbidden", "denied".into())]).await?;
    let provider = provider(base_url)?;
    assert!(provider.cache_models().await.is_err());
    assert!(provider.list_models().await.is_empty());
    server.await??;
    Ok(())
}

#[tokio::test]
async fn pricing_uses_discovered_base_model_for_reasoning_variants() -> Result<()> {
    let provider = provider(String::from("http://127.0.0.1/backend-api"))?;
    *provider.models.write().await = DiscoveredModels::new(vec![serde_json::from_value(json!({
        "slug": "gpt-6-astra",
        "display_name": "Astra",
        "visibility": "list",
        "default_reasoning_level": "low",
        "supported_reasoning_levels": [{"effort": "low", "description": "Fast"}]
    }))?])?;
    assert_eq!(
        provider
            .pricing_model_id(&ModelId::new("gpt-6-astra-low"))
            .await?,
        ModelId::new("gpt-6-astra")
    );
    Ok(())
}

#[test]
fn private_http_proxy_requires_explicit_opt_in_and_proxy_authentication() {
    for address in ["192.168.1.10", "10.88.0.1", "169.254.1.2", "[fd00::1]"] {
        let url = format!("http://{address}:1234/backend-api");
        assert!(!valid_backend_url(&url, true, false));
        assert!(!valid_backend_url(&url, false, true));
        assert!(valid_backend_url(&url, true, true));
        let mut config = DynamicConfig::from([
            ("base_url".into(), DynamicValue::String(url)),
            ("allow_http_proxy".into(), DynamicValue::Bool(true)),
            (
                "proxy_token_env".into(),
                DynamicValue::String("TEST_PROXY_TOKEN".into()),
            ),
        ]);
        assert!(OpenAiCodexFactory::validate_provider_config(&config).is_empty());
        config.remove("proxy_token_env");
        assert!(!OpenAiCodexFactory::validate_provider_config(&config).is_empty());
    }
    for url in ["http://example.com", "http://8.8.8.8", "http://0.0.0.0"] {
        assert!(!valid_backend_url(url, true, true));
    }
}

#[tokio::test]
async fn rejected_reasoning_retries_once_without_losing_visible_history() -> Result<()> {
    for reject_again in [false, true] {
        let rejection =
            json!({"error":{"code":"invalid_encrypted_content","message":"The encrypted content for item rs-old could not be verified."}})
                .to_string();
        let (base_url, server) = server(vec![
            ("200 OK", json!({"models":[{"slug":"plain","display_name":"Plain","visibility":"list","default_reasoning_level":"low","supported_reasoning_levels":[{"effort":"low","description":"Low"}]},{"slug":"non-reasoning","display_name":"Plain","visibility":"list","supported_reasoning_levels":[]}]}).to_string()),
            ("400 Bad Request", rejection.clone()),
            (if reject_again { "400 Bad Request" } else { "200 OK" }, if reject_again { rejection } else { "data: [DONE]\n\n".into() }),
            ("200 OK", "data: [DONE]\n\n".into()),
            ("200 OK", "data: [DONE]\n\n".into()),
        ]).await?;
        let provider = provider(base_url)?;
        provider.cache_models().await?;
        let request = ProviderRequest {
            cacheable_messages: None,
            script_tool: None,
            messages: vec![
                ConversationItem::Assistant {
                    items: vec![
                        kraai_types::AssistantItem::Reasoning {
                            provider_id: ProviderId::new("test-codex"),
                            payload: json!({"type":"reasoning","id":"rs-old","encrypted_content":"old-account","summary":[]}),
                        },
                        kraai_types::AssistantItem::Text {
                            phase: AssistantPhase::FinalAnswer,
                            text: "Keep this answer".into(),
                        },
                    ],
                },
                ConversationItem::User {
                    text: "Continue".into(),
                },
            ],
        };
        let response = provider
            .generate_reply_stream(
                &ModelId::new("plain"),
                request.clone(),
                &ProviderRequestContext::default(),
            )
            .await;
        assert_eq!(response.is_err(), reject_again);
        for model in ["plain", "non-reasoning"] {
            let mut next = request.clone();
            if model == "non-reasoning" {
                for message in &mut next.messages {
                    if let ConversationItem::Assistant { items } = message {
                        for item in items {
                            if let kraai_types::AssistantItem::Reasoning { payload, .. } = item {
                                *payload = json!({"type":"reasoning","id":"rs-new","encrypted_content":"fresh","summary":[]});
                            }
                        }
                    }
                }
            }
            let _stream = provider
                .generate_reply_stream(
                    &ModelId::new(model),
                    next,
                    &ProviderRequestContext::default(),
                )
                .await?;
        }
        let requests = server.await??;
        let bodies = requests
            .iter()
            .skip(1)
            .map(|request| {
                let (_, body) = request
                    .split_once("\r\n\r\n")
                    .ok_or_else(|| eyre!("missing body"))?;
                Ok(serde_json::from_str::<Value>(body)?)
            })
            .collect::<Result<Vec<_>>>()?;
        let original = bodies.first().ok_or_else(|| eyre!("missing original"))?;
        let retried = bodies.get(1).ok_or_else(|| eyre!("missing retry"))?;
        assert!(original.get("include").is_some());
        let original_input = original["input"]
            .as_array()
            .ok_or_else(|| eyre!("missing input"))?;
        assert_eq!(
            retried["input"],
            json!(original_input.iter().skip(1).collect::<Vec<_>>())
        );
        assert_eq!(requests.len(), 5);
        for later in bodies.iter().skip(2) {
            assert_eq!(later["input"], retried["input"]);
        }
        assert!(
            bodies
                .last()
                .ok_or_else(|| eyre!("missing plain request"))?
                .get("include")
                .is_none()
        );
    }
    Ok(())
}

#[tokio::test]
async fn native_compaction_sends_trigger_and_replays_encrypted_checkpoint() -> Result<()> {
    let models = json!({"models": [{"slug":"astra", "display_name":"Astra", "visibility":"list", "context_window":272000, "default_reasoning_level":"low", "supported_reasoning_levels":[{"effort":"low"}]}]}).to_string();
    let payload = json!({"type":"compaction","id":"cmp-1","encrypted_content":"opaque-state"});
    let completed = "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":200,\"output_tokens\":20}}}\n\n";
    let body = format!(
        "data: {}\n\n{completed}",
        json!({"type":"response.output_item.done","item":payload})
    );
    let (base_url, server) = server(vec![
        ("200 OK", models),
        ("200 OK", body),
        ("200 OK", completed.into()),
    ])
    .await?;
    let provider = provider(base_url)?;
    provider.cache_models().await?;
    let reasoning =
        json!({"type":"reasoning","id":"rs-1","encrypted_content":"prior-reasoning","summary":[]});
    let request = ProviderRequest {
        messages: vec![
            ConversationItem::System {
                text: "system instructions".into(),
            },
            ConversationItem::User {
                text: "task".into(),
            },
            ConversationItem::Assistant {
                items: vec![kraai_types::AssistantItem::Reasoning {
                    provider_id: provider.id.clone(),
                    payload: reasoning.clone(),
                }],
            },
        ],
        script_tool: Some(ScriptToolDefinition {
            name: "kraai_nushell".into(),
            description: "Run script".into(),
        }),
        cacheable_messages: None,
    };
    let events = provider
        .compact_stream(
            &ModelId::new("astra-low"),
            request.clone(),
            &ProviderRequestContext::default(),
        )
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    assert!(events.iter().any(|event| matches!(event, ProviderStreamEvent::Compaction { payload: value } if value == &payload)));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Usage(_)))
    );
    let mut replay = request;
    replay.messages.pop();
    replay.messages.push(ConversationItem::Compaction {
        provider_id: provider.id.clone(),
        payload: payload.clone(),
    });
    provider
        .generate_reply_stream(
            &ModelId::new("astra-low"),
            replay,
            &ProviderRequestContext::default(),
        )
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    let requests = server.await??;
    let bodies = requests
        .iter()
        .skip(1)
        .map(|r| {
            serde_json::from_str::<Value>(
                r.split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or_default(),
            )
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let compact = bodies
        .first()
        .ok_or_else(|| eyre!("missing compact request"))?;
    assert_eq!(
        compact["input"].as_array().and_then(|input| input.last()),
        Some(&json!({"type":"compaction_trigger"}))
    );
    assert!(
        compact["input"]
            .as_array()
            .is_some_and(|items| items.contains(&reasoning))
    );
    assert_eq!(
        compact.pointer("/tools/0/name").and_then(Value::as_str),
        Some("kraai_nushell")
    );
    assert_eq!(compact["instructions"], "system instructions");
    let replay = bodies.get(1).ok_or_else(|| eyre!("missing replay"))?;
    assert!(
        replay["input"]
            .as_array()
            .is_some_and(|items| items.contains(&payload))
    );
    assert!(!replay["input"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["type"] == "compaction_trigger")
    }));
    Ok(())
}

#[tokio::test]
async fn native_compaction_does_not_retry_without_rejected_reasoning() -> Result<()> {
    let models = json!({"models": [{"slug":"astra", "display_name":"Astra", "visibility":"list", "context_window":272000, "default_reasoning_level":"low", "supported_reasoning_levels":[{"effort":"low"}]}]}).to_string();
    let (base_url, server) = server(vec![
        ("200 OK", models),
        (
            "400 Bad Request",
            json!({"error":{"code":"invalid_encrypted_content","message":"invalid reasoning"}})
                .to_string(),
        ),
    ])
    .await?;
    let provider = provider(base_url)?;
    provider.cache_models().await?;
    let request = ProviderRequest {
        messages: vec![ConversationItem::Assistant {
            items: vec![kraai_types::AssistantItem::Reasoning {
                provider_id: provider.id.clone(),
                payload: json!({"type":"reasoning","id":"rs-1","encrypted_content":"opaque","summary":[]}),
            }],
        }],
        script_tool: None,
        cacheable_messages: None,
    };
    assert!(
        provider
            .compact_stream(
                &ModelId::new("astra-low"),
                request,
                &ProviderRequestContext::default()
            )
            .await
            .is_err()
    );
    assert_eq!(server.await??.len(), 2);
    Ok(())
}
