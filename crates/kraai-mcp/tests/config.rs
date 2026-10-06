#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "configuration fixtures assert parsing and availability"
)]

use kraai_mcp::{McpConfig, McpHost, McpManager, TransportConfig};
use kraai_types::McpRequest;

#[tokio::test]
async fn config_defaults_and_relative_working_directory_are_resolved() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp.toml");
    let missing = McpConfig::load(&path).await.unwrap();
    assert_eq!(missing.prompt_max_bytes, 16384);
    assert!(missing.servers.is_empty());
    tokio::fs::write(
        &path,
        r#"
prompt_max_bytes = 1024
[servers.local]
description = "Local tools"
transport = { type = "stdio", command = "fixture", args = ["--stdio"], cwd = "tools" }
[servers.remote]
transport = { type = "http", url = "https://example.test/mcp", bearer_token_env = "MCP_TOKEN" }
"#,
    )
    .await
    .unwrap();
    let config = McpConfig::load(&path).await.unwrap();
    assert_eq!(config.prompt_max_bytes, 1024);
    let local = &config.servers["local"];
    assert!(local.enabled);
    assert_eq!(local.call_timeout_secs, 60);
    let TransportConfig::Stdio { cwd, .. } = &local.transport else {
        panic!("wrong transport")
    };
    assert_eq!(*cwd, Some(directory.path().join("tools")));
}

#[tokio::test]
async fn disabled_servers_are_not_discovered_or_callable() {
    let config: McpConfig = toml::from_str(
        r#"
[servers.disabled]
enabled = false
transport = { type = "stdio", command = "does-not-exist-kraai-mcp" }
"#,
    )
    .unwrap();
    let manager = McpManager::new(config).unwrap();
    let prompt = manager.prompt().await;
    assert!(prompt.text.is_none());
    assert!(prompt.warnings.is_empty());
    assert!(
        manager
            .execute(McpRequest::Call {
                server: "disabled".into(),
                tool: "run".into(),
                arguments: Default::default()
            })
            .await
            .unwrap_err()
            .contains("not enabled")
    );
}

#[tokio::test]
async fn unavailable_servers_report_warnings_without_failing_the_prompt() {
    let config: McpConfig = toml::from_str(
        r#"
[servers.missing]
transport = { type = "stdio", command = "does-not-exist-kraai-mcp" }
"#,
    )
    .unwrap();
    let manager = McpManager::new(config).unwrap();
    let prompt = manager.prompt().await;
    assert!(prompt.text.unwrap().contains("missing"));
    assert_eq!(prompt.warnings.len(), 1);
    manager.shutdown().await;
}

#[test]
fn invalid_timeouts_and_urls_are_rejected() {
    for body in [
        "[servers.invalid]\ncall_timeout_secs = 0\ntransport = { type = 'stdio', command = 'fixture' }",
        "[servers.invalid]\ntransport = { type = 'http', url = 'file:///tmp/mcp' }",
    ] {
        let config: McpConfig = toml::from_str(body).unwrap();
        assert!(McpManager::new(config).is_err());
    }
}

#[test]
fn invalid_and_case_duplicate_http_headers_are_rejected_without_credentials() {
    for headers in [
        serde_json::json!({"bad header":"secret"}),
        serde_json::json!({"X-Token":"secret\r\nInjected: value"}),
        serde_json::json!({"X-Token":"secret\0"}),
        serde_json::json!({"X-Token":"secret", "x-token":"other-secret"}),
    ] {
        let config: McpConfig = serde_json::from_value(serde_json::json!({"servers":{"remote":{"transport":{"type":"http","url":"https://example.test/mcp","headers":headers}}}})).unwrap();
        let error = config.validate().unwrap_err();
        assert!(!error.contains("secret"));
    }
}

#[test]
fn explicit_authorization_rejects_competing_credential_sources() {
    for (setting, value) in [
        ("bearer_token_env", serde_json::json!("TOKEN")),
        ("oauth", serde_json::json!({})),
    ] {
        let mut transport = serde_json::json!({"type":"http","url":"https://example.test/mcp","headers":{"aUtHoRiZaTiOn":"Bearer secret"}});
        transport[setting] = value;
        let config: McpConfig = serde_json::from_value(
            serde_json::json!({"servers":{"remote":{"transport":transport}}}),
        )
        .unwrap();
        assert!(
            config
                .validate()
                .unwrap_err()
                .contains("Authorization header conflicts")
        );
    }
}

#[test]
fn session_aliases_cannot_shadow_enabled_or_disabled_configuration() {
    for enabled in [true, false] {
        let config: McpConfig = serde_json::from_value(serde_json::json!({"servers":{"configured":{"enabled":enabled,"transport":{"type":"stdio","command":"unused"}}}})).unwrap();
        let manager = McpManager::new(config.clone()).unwrap();
        assert!(manager.with_session_servers(config).is_err());
        let attached: McpConfig = serde_json::from_value(serde_json::json!({"servers":{"temporary":{"transport":{"type":"stdio","command":"unused"}}}})).unwrap();
        let overlay = manager.with_session_servers(attached.clone()).unwrap();
        assert!(overlay.with_session_servers(attached).is_err());
    }
}
