use super::*;
use kraai_types::McpRequest;
use serde_json::{Value, json};

struct FixtureMcp;

#[async_trait::async_trait]
impl kraai_mcp::McpHost for FixtureMcp {
    async fn execute(&self, request: McpRequest) -> Result<Value, String> {
        match request {
            McpRequest::Call {
                server,
                tool,
                arguments,
            } => {
                assert_eq!(server, "fixture");
                if tool == "fail" {
                    Ok(
                        json!({"isError": true, "content": [{"type": "text", "text": "fixture tool failure"}]}),
                    )
                } else {
                    Ok(json!({"content": [], "structuredContent": arguments}))
                }
            }
            McpRequest::Search { query, limit } => {
                assert_eq!(query, "find issues");
                assert_eq!(limit, 2);
                Ok(
                    json!({"tools": [{"server": "fixture", "name": "echo", "inputSchema": {"type": "object"}}]}),
                )
            }
            _ => Err(String::from("Unexpected fixture request")),
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn mcp_works_without_sandbox_network_and_preserves_record_arguments() {
    let workspace = TestWorkspace::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("network fixture");
    listener.set_nonblocking(true).expect("nonblocking fixture");
    let address = listener.local_addr().expect("fixture address");
    let mut plan = plan(
        format!(
            "try {{ http get --max-time 1sec http://{address} | ignore }} catch {{ null }}; kraai-mcp call fixture echo {{items: [{{title: 'å' value: 42}}]}} | get structuredContent.items | first | to json --raw"
        ),
        &workspace,
    );
    plan.capabilities =
        SandboxCapabilities::new([SandboxCapability::WorkspaceRead]).expect("capabilities");
    plan.runtime_roots = sandbox_runtime_roots(host_executable());
    plan.active_commands = vec![String::from("kraai-mcp")];
    plan.mcp = Arc::new(FixtureMcp);
    let result = match execute(plan, CancellationToken::new()).await {
        Ok(result) => result,
        Err(RuntimeError::Sandbox(kraai_sandbox::SandboxError::SandboxUnavailable(_))) => return,
        Err(error) => panic!("MCP execution failed: {error}"),
    };
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    let result: Value = serde_json::from_slice(&result.output.stdout).expect("MCP output");
    assert_eq!(result, json!({"title": "å", "value": 42}));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[tokio::test]
async fn search_returns_schemas_in_one_call() {
    let workspace = TestWorkspace::new();
    let mut plan = plan(
        "kraai-mcp search 'find issues' --limit 2 | get tools.0.inputSchema.type",
        &workspace,
    );
    plan.active_commands = vec![String::from("kraai-mcp")];
    plan.mcp = Arc::new(FixtureMcp);
    let result = execute(plan, CancellationToken::new())
        .await
        .expect("MCP search");
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(String::from_utf8_lossy(&result.output.stdout).contains("object"));
}

#[tokio::test]
async fn tool_errors_stop_dependent_script_work() {
    let workspace = TestWorkspace::new();
    let mut plan = plan(
        "kraai-mcp call fixture fail {}; 'should not run' | save marker",
        &workspace,
    );
    plan.active_commands = vec![String::from("kraai-mcp")];
    plan.mcp = Arc::new(FixtureMcp);
    let result = execute(plan, CancellationToken::new())
        .await
        .expect("MCP failure");
    assert_ne!(
        result.output.termination,
        Termination::Exited { code: Some(0) }
    );
    assert!(String::from_utf8_lossy(&result.output.stderr).contains("fixture tool failure"));
    assert!(!workspace.0.join("marker").exists());
}

struct StalledMcp {
    entered: CancellationToken,
    dropped: CancellationToken,
}

#[async_trait::async_trait]
impl kraai_mcp::McpHost for StalledMcp {
    async fn execute(&self, _: McpRequest) -> Result<Value, String> {
        let _guard = self.dropped.clone().drop_guard();
        self.entered.cancel();
        std::future::pending().await
    }
}

#[tokio::test]
async fn cancelling_execution_drops_the_mcp_request()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = TestWorkspace::new();
    let entered = CancellationToken::new();
    let dropped = CancellationToken::new();
    let mut plan = plan("kraai-mcp call fixture wait {}", &workspace);
    plan.active_commands = vec![String::from("kraai-mcp")];
    plan.mcp = Arc::new(StalledMcp {
        entered: entered.clone(),
        dropped: dropped.clone(),
    });
    let cancellation = CancellationToken::new();
    let mut task = Box::pin(execute(plan, cancellation.clone()));
    tokio::select! {
        result = &mut task => return Err(format!("MCP call exited early: {result:?}").into()),
        result = tokio::time::timeout(Duration::from_secs(5), entered.cancelled()) => result?,
    }
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task).await??;
    if result.output.termination != Termination::Cancelled {
        return Err("MCP script did not cancel".into());
    }
    tokio::time::timeout(Duration::from_secs(5), dropped.cancelled()).await?;
    Ok(())
}
