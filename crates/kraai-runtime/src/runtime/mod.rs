mod builder;
mod cache_warming;
mod client_view;
mod config;
mod core;
mod dispatch;
mod model_catalog;
mod nushell_host;
mod request_usage;
mod script_environment;
mod script_execution;
mod script_recovery;
mod session;
mod session_maintenance;
mod state_effects;
mod stream_driver;
mod stream_tasks;

pub use builder::RuntimeBuilder;

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::map_err_ignore,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_used,
    reason = "integration-style runtime tests use direct assertions and fixtures"
)]
mod tests;

mod images;
mod mcp_auth;
mod mcp_sessions;
