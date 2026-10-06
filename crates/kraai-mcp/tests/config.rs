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
