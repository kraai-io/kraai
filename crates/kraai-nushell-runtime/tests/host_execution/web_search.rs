use super::*;

#[cfg(target_os = "linux")]
struct FixtureSearch;

#[async_trait::async_trait]
#[cfg(target_os = "linux")]
impl kraai_web::WebSearch for FixtureSearch {
    async fn search(
        &self,
        request: &kraai_types::WebSearchRequest,
    ) -> Result<kraai_types::WebSearchResponse, String> {
        assert_eq!(request.query, "test query");
        assert_eq!(request.limit, 3);
        assert_eq!(request.max_chars, 100);
        Ok(kraai_types::WebSearchResponse {
            provider: String::from("exa"),
            content: String::from("fixture search result"),
            truncated: false,
        })
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn web_search_works_without_sandbox_network_access() {
    let workspace = TestWorkspace::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("network fixture");
    listener.set_nonblocking(true).expect("nonblocking fixture");
    let address = listener.local_addr().expect("fixture address");
    let mut execution = plan(
        format!("try {{ http get --max-time 1sec http://{address} | ignore }} catch {{}}; kraai-web-search 'test query' --limit 3 --max-chars 100 | to json --raw").into_bytes(),
        &workspace,
    );
    execution.capabilities =
        SandboxCapabilities::new([SandboxCapability::WorkspaceRead]).expect("capabilities");
    execution.runtime_roots = sandbox_runtime_roots(host_executable());
    execution.active_commands = vec![String::from("kraai-web-search")];
    execution.web_search = Arc::new(FixtureSearch);
    let result = match execute(execution, CancellationToken::new()).await {
        Ok(result) => result,
        Err(RuntimeError::Sandbox(kraai_sandbox::SandboxError::SandboxUnavailable(_))) => return,
        Err(error) => panic!("sandboxed search failed: {error}"),
    };
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&result.output.stdout).expect("search record");
    assert_eq!(value["content"], "fixture search result");
    assert_eq!(value["truncated"], false);
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

struct StalledSearch {
    entered: CancellationToken,
    dropped: CancellationToken,
}

#[async_trait::async_trait]
impl kraai_web::WebSearch for StalledSearch {
    async fn search(
        &self,
        _: &kraai_types::WebSearchRequest,
    ) -> Result<kraai_types::WebSearchResponse, String> {
        let _guard = self.dropped.clone().drop_guard();
        self.entered.cancel();
        std::future::pending().await
    }
}

#[tokio::test]
async fn cancelling_execution_drops_host_search()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = TestWorkspace::new();
    let entered = CancellationToken::new();
    let dropped = CancellationToken::new();
    let mut execution = plan(b"kraai-web-search 'test'".to_vec(), &workspace);
    execution.active_commands = vec![String::from("kraai-web-search")];
    execution.web_search = Arc::new(StalledSearch {
        entered: entered.clone(),
        dropped: dropped.clone(),
    });
    let cancellation = CancellationToken::new();
    let mut task = Box::pin(execute(execution, cancellation.clone()));
    tokio::select! {
        result = &mut task => return Err(format!("search exited early: {result:?}").into()),
        result = tokio::time::timeout(Duration::from_secs(5), entered.cancelled()) => result?,
    }
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task).await??;
    if result.output.termination != Termination::Cancelled {
        return Err(format!("unexpected termination: {:?}", result.output.termination).into());
    }
    tokio::time::timeout(Duration::from_secs(5), dropped.cancelled()).await?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "contacts the public anonymous Exa endpoint"]
async fn live_anonymous_exa_search_without_sandbox_network() {
    let workspace = TestWorkspace::new();
    let mut execution = plan(b"kraai-web-search 'Nushell custom commands official documentation' --limit 3 --max-chars 4000 | to json --raw".to_vec(), &workspace);
    execution.capabilities =
        SandboxCapabilities::new([SandboxCapability::WorkspaceRead]).expect("capabilities");
    execution.runtime_roots = sandbox_runtime_roots(host_executable());
    execution.active_commands = vec![String::from("kraai-web-search")];
    let result = execute(execution, CancellationToken::new())
        .await
        .expect("live search");
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&result.output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&result.output.stdout).expect("search record");
    assert_eq!(value["provider"], "exa");
    let content = value["content"].as_str().expect("text content");
    assert!(content.contains("nushell.sh"), "{content}");
    assert!(content.chars().count() <= 4000);
}
