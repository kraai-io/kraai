#![cfg(unix)]

mod support;

use std::time::Duration;

use kraai_nushell_runtime::{RuntimeError, execute};
use kraai_test_support::{HANG_TIMEOUT, hang_guard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

async fn timed_out_host(
    greeting: Option<&[u8]>,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = support::FakeHost::new(timeout, b"'must not execute'".to_vec())?;
    if fixture.plan.startup_timeout != Duration::from_secs(5) {
        return Err("default startup timeout changed".into());
    }
    rejected_host(fixture, greeting, "handshake timed out").await
}

async fn invalid_host(
    greeting: &[u8],
    expected: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut fixture = support::FakeHost::new(HANG_TIMEOUT * 2, b"'must not execute'".to_vec())?;
    fixture.plan.startup_timeout = HANG_TIMEOUT * 2;
    rejected_host(fixture, Some(greeting), expected).await
}

async fn rejected_host(
    fixture: support::FakeHost,
    greeting: Option<&[u8]>,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    let (result, client_result) = hang_guard(async { tokio::join!(execution, client) }).await?;
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
    timed_out_host(Some(&[1]), Duration::from_secs(1)).await
}

#[tokio::test]
async fn host_without_a_connection_fails_during_startup()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    timed_out_host(None, Duration::from_secs(30)).await
}

#[tokio::test]
async fn host_without_a_greeting_fails_during_startup()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    timed_out_host(Some(&[]), Duration::from_secs(30)).await
}

#[tokio::test]
async fn host_with_an_unversioned_greeting_fails_during_startup()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    timed_out_host(Some(&[1]), Duration::from_secs(30)).await
}

#[tokio::test]
async fn host_with_an_incompatible_version_is_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    invalid_host(&[1, 0, 0, 0, 0], "incompatible Nushell host protocol").await
}

#[tokio::test]
async fn host_with_an_invalid_greeting_is_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    invalid_host(&[255], "invalid transport greeting").await
}
