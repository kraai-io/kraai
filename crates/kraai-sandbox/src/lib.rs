#![deny(unsafe_code)]

mod config;
mod error;
mod output;
mod platform;
mod process;
mod temp_dir;

pub use config::{LaunchPlan, PrivateTempConfig};
pub use error::SandboxError;
pub use output::{ExecutionOutput, OutputEvent, OutputStream, Termination};
pub use process::run;

#[cfg(test)]
use platform::linux::{
    BWRAP_SECCOMP_STDIN_FD, SeccompInstruction, build_bwrap_args, build_bwrap_probe_args,
    bwrap_probe_failure_message, find_bwrap, restricted_network_seccomp_program,
    run_bwrap_sandbox_probe,
};
#[cfg(test)]
use process::is_likely_sandbox_denied;

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "sandbox tests assert argv construction and process behavior directly"
)]
mod tests;
