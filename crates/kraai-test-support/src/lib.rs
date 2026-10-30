//! Limits and fixtures shared by tests.
//!
//! Tests run next to other builds on shared machines, where disk and CPU latency vary by
//! orders of magnitude. A wall-clock bound in a test only detects a hang: it is never an
//! assertion about speed, so it sits far above any latency a working run can see.

use std::future::IntoFuture;
use std::time::Duration;

pub mod http;

/// How long a test waits for something that must eventually happen before declaring a hang.
pub const HANG_TIMEOUT: Duration = Duration::from_secs(120);

/// A product duration no test outlives, for timers a test must never see expire.
pub const OUTLIVES_ANY_TEST: Duration = HANG_TIMEOUT.saturating_mul(10);

/// Awaits `future`, failing only if it hangs.
pub fn hang_guard<F: IntoFuture>(future: F) -> tokio::time::Timeout<F::IntoFuture> {
    tokio::time::timeout(HANG_TIMEOUT, future)
}
