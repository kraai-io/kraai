use std::sync::atomic::Ordering;

use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;

use crate::support::{Harness, Reply, prompt};

#[tokio::test]
async fn eof_with_a_nonreading_client_still_terminates() -> Result<()> {
    let mut harness = Harness::new(vec![Reply::Text("large output ".repeat(32 * 1024))]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    let buffered = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        harness.output.fill_buf(),
    )
    .await??;
    assert!(!buffered.is_empty());
    assert!(
        String::from_utf8_lossy(buffered).contains("large output"),
        "unexpected first output: {}",
        String::from_utf8_lossy(buffered)
    );
    drop(harness.input.take());
    let status =
        tokio::time::timeout(std::time::Duration::from_secs(15), harness.child.wait()).await??;
    assert!(!status.success());
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn eof_stops_active_stream_and_restart_does_not_resume_it() -> Result<()> {
    let mut harness = Harness::new(vec![Reply::Streaming, "After EOF".into()]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    harness
        .until(|value| value.pointer("/params/update/content/text") == Some(&json!("partial")))
        .await?;
    harness.restart().await?;
    harness.initialize().await?;
    let replay = harness
        .request(
            3,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    assert!(
        replay
            .iter()
            .any(|value| value.pointer("/params/update/sessionUpdate")
                == Some(&json!("user_message_chunk")))
    );
    assert!(
        !replay
            .iter()
            .any(|value| value.pointer("/params/update/content/text") == Some(&json!("partial")))
    );
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 1);
    let values = harness
        .request(4, "session/prompt", prompt(&session))
        .await?;
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text") == Some(&json!("After EOF")))
    );
    harness.stop().await
}

#[tokio::test]
async fn eof_stops_an_active_embedded_script() -> Result<()> {
    let mut harness = Harness::new(vec![Reply::Script("# timeout=60sec permissions=no-sandbox\nsleep 30sec; 'should not exist' | save marker.txt".into()), "After script EOF".into()]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&session)}))
        .await?;
    let pending = harness
        .until(|value| value.get("method") == Some(&json!("session/request_permission")))
        .await?;
    let approval = pending.last().ok_or_else(|| eyre!("missing approval"))?;
    harness.send(json!({"jsonrpc":"2.0","id":approval["id"],"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})).await?;
    harness
        .until(|value| value.pointer("/params/update/status") == Some(&json!("in_progress")))
        .await?;
    harness.restart().await?;
    harness.initialize().await?;
    let replay = harness
        .request(
            3,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    assert!(
        replay
            .iter()
            .any(|value| value.pointer("/params/update/status") == Some(&json!("failed")))
    );
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 1);
    assert!(!harness.root.path().join("marker.txt").exists());
    let values = harness
        .request(4, "session/prompt", prompt(&session))
        .await?;
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text")
                == Some(&json!("After script EOF")))
    );
    assert_eq!(harness.model_requests.load(Ordering::SeqCst), 2);
    harness.stop().await
}

#[tokio::test]
async fn pending_permission_is_isolated_from_other_session_traffic() -> Result<()> {
    let mut harness = Harness::new(vec![
        Reply::Script("# timeout=10sec permissions=no-sandbox\nprint 'approved'".into()),
        Reply::Chunks(2048),
        "After approval".into(),
    ])
    .await?;
    harness.initialize().await?;
    let first = harness.session().await?;
    let second = harness.session().await?;
    harness
        .send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":prompt(&first)}))
        .await?;
    let pending = harness
        .until(|value| value.get("method") == Some(&json!("session/request_permission")))
        .await?;
    let approval = pending.last().ok_or_else(|| eyre!("missing approval"))?;
    let values = harness
        .request(3, "session/prompt", prompt(&second))
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn")),
        "last response: {:?}",
        values.last()
    );
    let text = values
        .iter()
        .filter_map(|value| {
            value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        })
        .collect::<String>();
    for index in 0..2048 {
        assert!(text.contains(&format!("chunk-{index} ")));
    }
    harness.send(json!({"jsonrpc":"2.0","id":approval["id"],"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})).await?;
    let values = harness
        .until(|value| value.get("id") == Some(&json!(2)))
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("end_turn"))
    );
    assert!(values.iter().any(
        |value| value.pointer("/params/update/content/text") == Some(&json!("After approval"))
    ));
    harness.stop().await
}

#[tokio::test]
async fn cancellation_is_isolated_between_concurrent_sessions() -> Result<()> {
    let mut harness = Harness::new(vec![
        Reply::Streaming,
        Reply::Streaming,
        "Second still works".into(),
    ])
    .await?;
    harness.initialize().await?;
    let first = harness.session().await?;
    let second = harness.session().await?;
    for (id, session) in [(2, &first), (3, &second)] {
        harness
            .send(
                json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":prompt(session)}),
            )
            .await?;
        harness
            .until(|value| {
                value.pointer("/params/sessionId") == Some(&json!(session))
                    && value.pointer("/params/update/content/text") == Some(&json!("partial"))
            })
            .await?;
    }
    harness
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":first}}))
        .await?;
    let values = harness
        .until(|value| value.get("id") == Some(&json!(2)))
        .await?;
    assert_eq!(
        values
            .last()
            .and_then(|value| value.pointer("/result/stopReason")),
        Some(&json!("cancelled"))
    );
    assert!(
        !values
            .iter()
            .any(|value| value.get("id") == Some(&json!(3)))
    );
    let busy = harness
        .request(4, "session/prompt", prompt(&second))
        .await?;
    assert!(busy.last().and_then(|value| value.get("error")).is_some());
    harness
        .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":second}}))
        .await?;
    harness
        .until(|value| value.get("id") == Some(&json!(3)))
        .await?;
    let values = harness
        .request(5, "session/prompt", prompt(&second))
        .await?;
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/update/content/text")
                == Some(&json!("Second still works")))
    );
    harness.stop().await
}
