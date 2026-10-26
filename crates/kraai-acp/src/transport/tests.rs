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
        &CancellationToken::new(),
        kraai_test_support::HANG_TIMEOUT,
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
    reason = "assertions validate output capacity waits"
)]
async fn capacity_waits_for_enough_space_and_cancellation_does_not_reserve()
-> color_eyre::Result<()> {
    let budget = Budget::new(1024);
    budget.reserve(1024)?;
    let mut waiting = Box::pin(budget.reserve_wait(512));
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    budget.release(256);
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    assert!(!budget.failed.is_cancelled());
    budget.release(256);
    kraai_test_support::hang_guard(waiting).await??;
    assert_eq!(budget.bytes.load(Ordering::Acquire), 1024);
    let mut cancelled = Box::pin(budget.reserve_wait(1));
    assert!(futures::poll!(cancelled.as_mut()).is_pending());
    drop(cancelled);
    budget.release(1024);
    budget.reserve_wait(1024).await?;
    assert_eq!(budget.bytes.load(Ordering::Acquire), 1024);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions validate output capacity failures"
)]
async fn closed_transport_and_oversized_frames_do_not_leave_capacity_waits_hanging()
-> color_eyre::Result<()> {
    let budget = Budget::new(1024);
    budget.reserve(1024)?;
    let mut waiting = Box::pin(budget.reserve_wait(1));
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    budget.failed.cancel();
    assert!(kraai_test_support::hang_guard(waiting).await?.is_err());
    assert_eq!(budget.bytes.load(Ordering::Acquire), 1024);
    let budget = Budget::new(1024);
    assert!(
        kraai_test_support::hang_guard(budget.reserve_wait(1025))
            .await?
            .is_err()
    );
    assert_eq!(budget.bytes.load(Ordering::Acquire), 0);
    assert!(budget.failed.is_cancelled());
    Ok(())
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
    let mut waiting = Box::pin(budget.reserve_wait(OUTPUT_BYTES));
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    let line = json!({"jsonrpc":"2.0","method":"session/update","params":params}).to_string();
    assert_eq!(budget.bytes.load(Ordering::Acquire), reservation);
    let mut written = futures::io::Cursor::new(Vec::new());
    write_frame(
        &mut written,
        &line,
        &budget,
        &CancellationToken::new(),
        kraai_test_support::HANG_TIMEOUT,
    )
    .await?;
    assert_eq!(budget.bytes.load(Ordering::Acquire), 0);
    assert_eq!(written.into_inner(), format!("{line}\n").into_bytes());
    kraai_test_support::hang_guard(waiting).await??;
    assert_eq!(budget.bytes.load(Ordering::Acquire), OUTPUT_BYTES);
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
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions validate when blocked output expires"
)]
async fn blocked_output_waits_for_the_client_until_its_input_closes() -> color_eyre::Result<()> {
    let budget = Budget::new(OUTPUT_BYTES);
    let closed = CancellationToken::new();
    let line = json!({"jsonrpc":"2.0","id":1,"result":{}}).to_string();
    let timeout = Duration::from_millis(10);
    let mut writer = BlockedWriter;
    let mut writing = Box::pin(write_frame(&mut writer, &line, &budget, &closed, timeout));
    assert!(futures::poll!(writing.as_mut()).is_pending());
    tokio::time::sleep(timeout * 5).await;
    assert!(futures::poll!(writing.as_mut()).is_pending());
    closed.cancel();
    let result = kraai_test_support::hang_guard(writing).await?;
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions validate when input counts as closed"
)]
async fn input_counts_as_closed_once_it_ends_or_is_dropped() -> color_eyre::Result<()> {
    let closed = CancellationToken::new();
    let mut input = Box::pin(cancel_when_finished(
        futures::stream::iter([1, 2]),
        closed.clone(),
    ));
    assert_eq!(input.next().await, Some(1));
    assert_eq!(input.next().await, Some(2));
    assert!(!closed.is_cancelled());
    assert_eq!(input.next().await, None);
    assert!(closed.is_cancelled());

    let closed = CancellationToken::new();
    let input = cancel_when_finished(futures::stream::pending::<()>(), closed.clone());
    drop(input);
    assert!(closed.is_cancelled());
    Ok(())
}
