#[path = "mcp_fixture.rs"]
mod fixture;

use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::{Harness, Reply};
use fixture::{HttpFixture, execute, load, session, tool_status};

fn script(value: &str) -> Reply {
    Reply::Script(format!(
        "# timeout=10sec permissions=no-sandbox\nkraai-mcp call attached echo {{value: '{value}'}}"
    ))
}

#[tokio::test]
async fn reconnect_attaches_only_the_servers_supplied_by_the_client() -> Result<()> {
    let fixture = HttpFixture::new("ACP reconnected HTTP echo").await?;
    let mut harness = Harness::new(vec![
        script("before restart"),
        "done".into(),
        script("not attached"),
        "unavailable".into(),
        script("reattached"),
        "done again".into(),
    ])
    .await?;
    harness.initialize().await?;
    let id = session(&mut harness, json!([fixture.config()])).await?;
    assert!(tool_status(&execute(&mut harness, &id).await?, "completed"));
    harness.restart().await?;
    harness.initialize().await?;
    load(&mut harness, &id, json!([])).await?;
    assert!(tool_status(&execute(&mut harness, &id).await?, "failed"));
    load(&mut harness, &id, json!([fixture.config()])).await?;
    assert!(tool_status(&execute(&mut harness, &id).await?, "completed"));
    let calls = fixture.calls.lock().await.clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls
            .last()
            .and_then(|call| call.pointer("/params/arguments/value")),
        Some(&json!("reattached"))
    );
    harness.stop().await
}

#[tokio::test]
async fn http_attachments_call_tools_with_headers_and_remain_session_scoped() -> Result<()> {
    let first = HttpFixture::new("ACP first HTTP echo").await?;
    let replacement = HttpFixture::new("ACP replacement HTTP echo").await?;
    let mut harness = Harness::new(vec![
        script("first"),
        "first done".into(),
        script("isolated"),
        "isolated done".into(),
        script("replacement"),
        "replacement done".into(),
        script("cleared"),
        "cleared done".into(),
    ])
    .await?;
    let initialized = harness.initialize().await?;
    assert_eq!(
        initialized.pointer("/result/agentCapabilities/mcpCapabilities/http"),
        Some(&json!(true))
    );
    assert_ne!(
        initialized.pointer("/result/agentCapabilities/mcpCapabilities/sse"),
        Some(&json!(true))
    );
    let attached = session(&mut harness, json!([first.config()])).await?;
    let isolated = session(&mut harness, json!([])).await?;
    assert!(tool_status(
        &execute(&mut harness, &attached).await?,
        "completed"
    ));
    assert!(
        harness
            .provider_payloads
            .lock()
            .await
            .first()
            .is_some_and(|payload| payload.to_string().contains("ACP first HTTP echo"))
    );
    assert!(tool_status(
        &execute(&mut harness, &isolated).await?,
        "failed"
    ));
    let payloads = harness.provider_payloads.lock().await.clone();
    assert!(
        payloads
            .get(2)
            .is_some_and(|payload| !payload.to_string().contains("ACP first HTTP echo"))
    );
    load(&mut harness, &attached, json!([replacement.config()])).await?;
    assert!(tool_status(
        &execute(&mut harness, &attached).await?,
        "completed"
    ));
    let payloads = harness.provider_payloads.lock().await.clone();
    let replacement_prompt = payloads
        .get(4)
        .ok_or_else(|| eyre!("missing replacement prompt"))?;
    let system = replacement_prompt
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role") == Some(&json!("system")))
        .map(Value::to_string)
        .collect::<String>();
    assert!(system.contains("ACP replacement HTTP echo"));
    assert!(!system.contains("ACP first HTTP echo"));
    load(&mut harness, &attached, json!([])).await?;
    assert!(tool_status(
        &execute(&mut harness, &attached).await?,
        "failed"
    ));
    let calls = first.calls.lock().await.clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls
            .first()
            .and_then(|call| call.pointer("/params/arguments/value")),
        Some(&json!("first"))
    );
    let calls = replacement.calls.lock().await.clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls
            .first()
            .and_then(|call| call.pointer("/params/arguments/value")),
        Some(&json!("replacement"))
    );
    harness.stop().await
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_attachment_receives_env_cwd_and_exits_on_clear_and_disconnect() -> Result<()> {
    let shell =
        std::env::split_paths(&std::env::var_os("PATH").ok_or_else(|| eyre!("PATH missing"))?)
            .map(|directory| directory.join("sh"))
            .find(|path| path.is_file())
            .ok_or_else(|| eyre!("sh unavailable"))?;
    let mut harness = Harness::new(vec![
        script("stdio"),
        "done".into(),
        script("after reload"),
        "done again".into(),
    ])
    .await?;
    harness.initialize().await?;
    let record = harness.root.path().join("mcp-record");
    let pid_file = harness.root.path().join("mcp-pid");
    let config = json!({"name":"attached","command":shell,"args":["-c",fixture::STDIO],"env":[{"name":"ACP_MCP_RECORD","value":record},{"name":"ACP_MCP_PID","value":pid_file},{"name":"ACP_MCP_VALUE","value":"stdio env received"}]});
    let id = session(&mut harness, json!([config.clone()])).await?;
    let values = execute(&mut harness, &id).await?;
    assert!(tool_status(&values, "completed"));
    assert!(values.iter().any(|value| {
        value
            .pointer("/params/update/content/0/content/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains("stdio env received"))
    }));
    assert!(
        harness
            .provider_payloads
            .lock()
            .await
            .first()
            .is_some_and(|payload| payload.to_string().contains("ACP stdio attached echo"))
    );
    assert_eq!(
        tokio::fs::read_to_string(&record).await?,
        format!(
            "{}\nstdio env received\ncalled\n",
            tokio::fs::canonicalize(harness.root.path())
                .await?
                .display()
        )
    );
    let pid = tokio::fs::read_to_string(&pid_file).await?;
    load(&mut harness, &id, json!([])).await?;
    wait_for_exit(&shell, pid.trim()).await?;
    load(&mut harness, &id, json!([config])).await?;
    assert!(tool_status(&execute(&mut harness, &id).await?, "completed"));
    let pid = tokio::fs::read_to_string(&pid_file).await?;
    harness.stop().await?;
    wait_for_exit(&shell, pid.trim()).await
}

#[cfg(unix)]
async fn wait_for_exit(shell: &std::path::Path, pid: &str) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if !tokio::process::Command::new(shell)
                .args(["-c", "kill -0 \"$1\" 2>/dev/null", "fixture", pid])
                .status()
                .await?
                .success()
            {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn invalid_attachment_replacement_leaves_existing_session_usable() -> Result<()> {
    let fixture = HttpFixture::new("ACP retained HTTP echo").await?;
    let mut harness = Harness::new(vec![script("retained"), "done".into()]).await?;
    harness.initialize().await?;
    let id = session(&mut harness, json!([fixture.config()])).await?;
    for servers in [
        json!([fixture.config(), fixture.config()]),
        json!([{"name":"sse","type":"sse","url":fixture.url,"headers":[]}]),
        json!([{"name":"relative","command":"relative","args":[],"env":[]}]),
        json!([{"name":"bad-env","command":"/unused","args":[],"env":[{"name":"","value":"x"}]}]),
        json!([{"name":"bad-header","type":"http","url":fixture.url,"headers":[{"name":"X-Test","value":"a\nb"}]}]),
        json!([{"name":"duplicate-header","type":"http","url":fixture.url,"headers":[{"name":"X-Test","value":"a"},{"name":"x-test","value":"b"}]}]),
        json!([{"name":"bad-url","type":"http","url":"file:///unused","headers":[]}]),
    ] {
        let values = harness
            .request(
                12,
                "session/load",
                json!({"sessionId":id,"cwd":harness.root.path(),"mcpServers":servers}),
            )
            .await?;
        assert_eq!(
            values.last().and_then(|value| value.pointer("/error/code")),
            Some(&json!(-32602)),
            "{values:?}"
        );
    }
    assert!(tool_status(&execute(&mut harness, &id).await?, "completed"));
    assert_eq!(fixture.calls.lock().await.len(), 1);
    harness.stop().await
}
