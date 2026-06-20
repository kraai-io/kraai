mod builder;
mod config;
mod core;
mod dispatch;
mod streaming;
mod tool_call_guard;
mod tool_calls;

pub use builder::RuntimeBuilder;

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::map_err_ignore,
    clippy::panic,
    clippy::panic_in_result_fn,
    reason = "integration-style runtime tests use direct assertions and fixtures"
)]
mod tests;
