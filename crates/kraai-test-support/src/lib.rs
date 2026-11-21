//! Test machines vary disk and CPU latency by orders of magnitude, so a wall-clock bound only
//! detects a hang and sits far above any latency a working run can see.

use std::future::IntoFuture;
use std::time::Duration;

pub mod http;

pub const HANG_TIMEOUT: Duration = Duration::from_secs(120);

pub const OUTLIVES_ANY_TEST: Duration = HANG_TIMEOUT.saturating_mul(10);

pub fn hang_guard<F: IntoFuture>(future: F) -> tokio::time::Timeout<F::IntoFuture> {
    tokio::time::timeout(HANG_TIMEOUT, future)
}
