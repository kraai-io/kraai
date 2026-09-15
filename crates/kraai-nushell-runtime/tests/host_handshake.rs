#![cfg(unix)]

mod support;

use std::time::Duration;

use kraai_nushell_runtime::{RuntimeError, execute};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

async fn rejected_host(
    greeting: Option<&[u8]>,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    rejected_host_with_timeout(greeting, expected, Duration::from_secs(30)).await
}

async fn rejected_host_with_timeout(
    greeting: Option<&[u8]>,
    expected: &str,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = support::FakeHost::new(timeout, b"'must not execute'".to_vec())?;
    let directory = fixture
        .socket
        .parent()
        .ok_or("missing transport directory")?
        .to_path_buf();
    let socket = fixture.socket;
    let plan = fixture.plan;
    let cancellation = CancellationToken::new();
    let _guard = cancellation.clone().drop_guard();
    let execution = execute(plan, cancellation);
    let client = async {
        let Some(greeting) = greeting else {
            return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
        };
        let mut stream = support::connect(&socket).await?;
        stream.write_all(greeting).await?;
        let mut request = Vec::new();
        stream.read_to_end(&mut request).await?;
        assert!(
            request.is_empty(),
            "an incompatible host received a script request"
        );
        Ok(())
    };
    let (result, client_result) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(execution, client)
    })
    .await?;
    client_result?;
    assert!(
        matches!(&result, Err(RuntimeError::Transport(message)) if message.contains(expected)),
        "unexpected host result: {result:?}"
    );
    assert!(
        !directory.exists(),
        "host cleanup did not finish before reporting the error"
    );
    Ok(())
}

#[tokio::test]
async fn short_script_timeout_does_not_hide_an_unversioned_host()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    rejected_host_with_timeout(Some(&[1]), "handshake timed out", Duration::from_secs(1)).await
}

#[tokio::test]
async fn host_without_a_connection_fails_during_startup()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    rejected_host(None, "handshake timed out").await
}

#[tokio::test]
async fn host_without_a_greeting_fails_during_startup()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    rejected_host(Some(&[]), "handshake timed out").await
}

#[tokio::test]
async fn host_with_an_unversioned_greeting_fails_during_startup()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    rejected_host(Some(&[1]), "handshake timed out").await
}

#[tokio::test]
async fn host_with_an_incompatible_version_is_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    rejected_host(Some(&[1, 0, 0, 0, 0]), "incompatible Nushell host protocol").await
}

#[tokio::test]
async fn host_with_an_invalid_greeting_is_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    rejected_host(Some(&[255]), "invalid transport greeting").await
}
