#![cfg(unix)]

mod support;

use std::time::Duration;

use kraai_nushell_runtime::{RuntimeError, execute, request::HOST_PROTOCOL_VERSION};
use kraai_sandbox::Termination;
use support::{FakeHost, TestResult};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

async fn greet(stream: &mut tokio::net::UnixStream) -> TestResult {
    stream.write_u8(1).await?;
    stream.write_u32(HOST_PROTOCOL_VERSION).await?;
    Ok(())
}

#[tokio::test]
async fn slow_compatible_host_receives_the_full_script_timeout() -> TestResult {
    let timeout = Duration::from_millis(100);
    let fixture = FakeHost::new(timeout, b"ignored".to_vec())?;
    let client = async {
        let mut stream = support::connect(&fixture.socket).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        greet(&mut stream).await?;
        let started = tokio::time::Instant::now();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).await?;
        if request.is_empty() || started.elapsed() < timeout {
            return Err("script timeout budget was consumed by host startup".into());
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    };
    let (result, client) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(execute(fixture.plan, CancellationToken::new()), client)
    })
    .await?;
    client?;
    let result = result?;
    if result.output.termination != Termination::TimedOut {
        return Err(format!("unexpected outcome: {result:?}").into());
    }
    Ok(())
}

async fn channel_failure(cancel: bool, during_request: bool) -> TestResult {
    let source = if during_request {
        vec![b'x'; 2 * 1024 * 1024]
    } else {
        b"ignored".to_vec()
    };
    let fixture = FakeHost::new(Duration::from_secs(30), source)?;
    let cancellation = CancellationToken::new();
    let _guard = cancellation.clone().drop_guard();
    let client = async {
        let mut stream = support::connect(&fixture.socket).await?;
        greet(&mut stream).await?;
        let length = usize::try_from(stream.read_u64().await?)?;
        if during_request {
            stream.read_u8().await?;
        } else {
            let mut request = vec![0; length];
            stream.read_exact(&mut request).await?;
            stream.write_u32(1024).await?;
            stream.write_all(b"{").await?;
            tokio::task::yield_now().await;
        }
        if cancel {
            cancellation.cancel();
        }
        drop(stream);
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    };
    let (result, client) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(execute(fixture.plan, cancellation.clone()), client)
    })
    .await?;
    client?;
    if cancel {
        let result = result?;
        if result.output.termination != Termination::Cancelled {
            return Err(format!("external cancellation was lost: {result:?}").into());
        }
    } else {
        let expected = if during_request {
            matches!(result, Err(RuntimeError::RequestChannel(_)))
        } else {
            matches!(result, Err(RuntimeError::HostChannel(_)))
        };
        if !expected {
            return Err(format!("internal cancellation hid a channel failure: {result:?}").into());
        }
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_during_request_io_remains_cancelled() -> TestResult {
    channel_failure(true, true).await
}

#[tokio::test]
async fn cancellation_during_effect_io_remains_cancelled() -> TestResult {
    channel_failure(true, false).await
}

#[tokio::test]
async fn request_failure_without_external_cancellation_remains_an_error() -> TestResult {
    channel_failure(false, true).await
}

#[tokio::test]
async fn effect_failure_without_external_cancellation_remains_an_error() -> TestResult {
    channel_failure(false, false).await
}
