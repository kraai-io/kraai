#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "stdio fixture asserts protocol results"
)]

use kraai_mcp::{McpConfig, McpHost, McpManager, ServerConfig, TransportConfig};
use kraai_types::McpRequest;

const FIXTURE: &str = r#"
printf 'started\n' >> "$MCP_RECORD"
tool=echo
while IFS= read -r line; do
    id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
    case "$line" in
        *server/discover*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"legacy"}}\n' "$id" ;;
        *initialize\"*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"fixture","version":"1.0.0"}}}\n' "$id" ;;
        *tools/list*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"%s","description":"Fixture echo","inputSchema":{"type":"object"}}]}}\n' "$id" "$tool" ;;
        *tools/call*) tool=changed; printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'; printf 'called\n' >> "$MCP_RECORD"; printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"fixture result"}],"structuredContent":{"ok":true}}}\n' "$id" ;;
    esac
done
"#;

#[tokio::test]
async fn stdio_negotiates_legacy_and_reuses_the_process() {
    let directory = tempfile::tempdir().unwrap();
    let record = directory.path().join("calls");
    let config = McpConfig {
        servers: [(
            "local".into(),
            ServerConfig {
                enabled: true,
                description: "Local fixture".into(),
                startup_timeout_secs: 3,
                call_timeout_secs: 3,
                transport: TransportConfig::Stdio {
                    command: "sh".into(),
                    args: vec!["-c".into(), FIXTURE.into()],
                    env: [("MCP_RECORD".into(), record.to_string_lossy().into_owned())].into(),
                    cwd: Some(directory.path().into()),
                },
            },
        )]
        .into(),
        ..Default::default()
    };
    let manager = McpManager::new(config).unwrap();
    let prompt = manager.prompt().await;
    assert!(prompt.warnings.is_empty(), "{:?}", prompt.warnings);
    for _ in 0..2 {
        let result = manager
            .execute(McpRequest::Call {
                server: "local".into(),
                tool: "echo".into(),
                arguments: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(result["structuredContent"]["ok"], true);
    }
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let tools = manager
                .execute(McpRequest::Tools {
                    server: "local".into(),
                })
                .await
                .unwrap();
            if tools[0]["name"] == "changed" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(record).await.unwrap(),
        "started\ncalled\ncalled\n"
    );
    manager.shutdown().await;
}
