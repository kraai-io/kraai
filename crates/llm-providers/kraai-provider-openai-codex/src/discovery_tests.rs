#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert HTTP behavior and propagate transport errors"
)]

use super::*;
use crate::auth::OpenAiCodexAuthControllerOptions;
use color_eyre::eyre::ensure;
use kraai_provider_core::ScriptToolDefinition;
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
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
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
        client: build_codex_http_client(true)?,
        models: RwLock::new(DiscoveredModels::default()),
        model_configs: BTreeMap::new(),
        base_url,
        proxy_token: Some("test-token".into()),
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
            assert_eq!(valid_backend_url(url, proxy_token), allowed, "{url}");
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
async fn proxy_redirect_cannot_bypass_backend_transport_validation() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = [0_u8; 4096];
        ensure!(
            stream.read(&mut buffer).await? > 0,
            "missing redirect request"
        );
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://example.com/backend-api/codex/models\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        Ok::<_, color_eyre::Report>(())
    });
    let provider = provider(format!("http://{address}/backend-api"))?;
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
    let result = build_codex_http_client(false)?
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
    assert_eq!(model.name, "Brand New high");
    assert_eq!(model.max_context, Some(234567));
    let request = ProviderRequest {
        messages: vec![],
        script_tool: Some(ScriptToolDefinition {
            name: "kraai_nushell".into(),
            description: "Run a script".into(),
        }),
    };
    provider
        .send_responses_request(&model.id, request, &ProviderRequestContext::default())
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
