mod bash_habits;
mod builder;
mod client_view;
mod config;
mod core;
mod dispatch;
mod merged_calls;
mod model_catalog;
mod model_lists;
mod nushell_host;
mod request_usage;
mod script_environment;
mod script_execution;
mod script_output_tail;
mod script_recovery;
mod session;
mod session_maintenance;
mod state_effects;
mod stream_driver;
mod stream_tasks;

pub use builder::RuntimeBuilder;

#[cfg(test)]
mod tests;

mod images;
mod mcp_auth;
mod mcp_sessions;
