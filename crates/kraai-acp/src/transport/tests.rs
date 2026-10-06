use std::pin::Pin;
use std::task::{Context, Poll};

use serde_json::json;

use super::*;

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions validate byte accounting after fallible I/O"
)]
async fn fractional_metadata_releases_exactly_the_admitted_bytes() -> color_eyre::Result<()> {
    let budget = Budget::new(OUTPUT_BYTES);
    let params = r#"{"sessionId":"session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"🦀\n\t"}},"_meta":{"fraction":52.314008204106244}}"#;
    let reservation = params.len() + ENVELOPE_BYTES;
    budget.reserve(reservation)?;
    let line = format!(r#"{{"jsonrpc":"2.0","method":"session/update","params":{params}}}"#);
    write_frame(
        &mut futures::io::Cursor::new(Vec::new()),
        &line,
        &budget,
        Duration::from_secs(5),
    )
    .await?;
    assert_eq!(budget.bytes.load(Ordering::Acquire), 0);
    Ok(())
}

#[test]
fn admission_never_exceeds_capacity_and_overload_stays_failed() {
    let budget = Budget::new(1024);
    for _ in 0..8 {
        assert!(budget.reserve(128).is_ok());
    }
    assert!(budget.reserve(1).is_err());
    assert_eq!(budget.bytes.load(Ordering::Acquire), 1024);
    budget.release(1024);
    assert!(budget.reserve(1).is_err());
    assert!(budget.failed.is_cancelled());
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions validate byte accounting after fallible I/O"
)]
async fn large_image_reservation_is_released_only_after_physical_flush() -> color_eyre::Result<()> {
    let budget = Budget::new(OUTPUT_BYTES);
    let params = acp::SessionNotification::new(
        acp::SessionId::new("session"),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Image(
            acp::ImageContent::new("A".repeat(8 * 1024 * 1024), "image/png"),
        ))),
    );
    let reservation = serialized_size(&params)? + ENVELOPE_BYTES;
    budget.reserve(reservation)?;
    let line = json!({"jsonrpc":"2.0","method":"session/update","params":params}).to_string();
    assert_eq!(budget.bytes.load(Ordering::Acquire), reservation);
    let mut written = futures::io::Cursor::new(Vec::new());
    write_frame(&mut written, &line, &budget, Duration::from_secs(5)).await?;
    assert_eq!(budget.bytes.load(Ordering::Acquire), 0);
    assert_eq!(written.into_inner(), format!("{line}\n").into_bytes());
    Ok(())
}

struct BlockedWriter;

impl AsyncWrite for BlockedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Pending
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Pending
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

#[tokio::test]
async fn blocked_stdout_expires_instead_of_hanging_shutdown() {
    let budget = Budget::new(OUTPUT_BYTES);
    let result = write_frame(
        &mut BlockedWriter,
        &json!({"jsonrpc":"2.0","id":1,"result":{}}).to_string(),
        &budget,
        Duration::from_millis(10),
    )
    .await;
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
}
