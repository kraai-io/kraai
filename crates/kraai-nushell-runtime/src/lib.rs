mod bash_habits;
mod effects;
mod execution;
mod host;
mod host_calls;
mod images;
mod rendering;
pub mod request;
mod startup_files;
mod transport;
mod wire;

pub use bash_habits::{BashRewrite, rewrite_bash_habits};
pub use effects::{RejectStateEffects, StateEffectHandler};
pub use execution::{RuntimeError, ScriptExecutionPlan, ScriptExecutionResult, execute};
pub use images::{ImageAttachmentHandler, RejectImageAttachments};
pub use startup_files::{StartupFile, startup_files};

#[doc(hidden)]
pub const INTERNAL_HOST_ARGUMENT: &str = "--kraai-internal-nushell-host";

#[doc(hidden)]
pub fn run_host_process() -> i32 {
    let transport_path = match host_transport_path() {
        Ok(path) => path,
        Err(message) => {
            report_host_error(message);
            return 64;
        }
    };
    let transport = match transport::connect(&transport_path) {
        Ok(transport) => transport,
        Err(error) => {
            report_host_error(error);
            return 70;
        }
    };
    let (request, transport) = match wire::read_request(transport) {
        Ok(request) => request,
        Err(error) => {
            report_host_error(error);
            return 70;
        }
    };
    #[cfg(target_os = "linux")]
    if let Err(error) = match request.startup_restriction {
        request::StartupRestriction::None => Ok(()),
        request::StartupRestriction::Network => kraai_sandbox::restrict_network_after_startup(),
        request::StartupRestriction::HostIpc => kraai_sandbox::restrict_host_ipc_after_startup(),
    } {
        report_host_error(error);
        return 70;
    }
    let host_client = std::sync::Arc::new(host_calls::DescriptorClient::from_transport(
        request.execution_id.clone(),
        request.event_secret,
        transport,
    ));
    let context = kraai_commands::CommandContext::new(
        host_client.clone(),
        host_client.clone(),
        host_client.clone(),
        host_client.clone(),
    );
    let registry = match kraai_commands::command_registry(context) {
        Ok(registry) => registry,
        Err(error) => {
            report_host_error(format!("invalid built-in command registry: {error}"));
            return 70;
        }
    };
    match host::run_request(request, &registry, host_client) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            report_host_error(error);
            70
        }
    }
}

fn report_host_error(message: impl std::fmt::Display) {
    use std::io::Write;

    drop(writeln!(
        std::io::stderr().lock(),
        "kraai-nushell-host: {message}"
    ));
}

fn host_transport_path() -> Result<std::path::PathBuf, String> {
    host_transport_path_from(std::env::args_os().skip(1))
}

fn host_transport_path_from(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<std::path::PathBuf, String> {
    let mut flag = args
        .next()
        .ok_or_else(|| String::from("missing --transport argument"))?;
    if flag == INTERNAL_HOST_ARGUMENT {
        flag = args
            .next()
            .ok_or_else(|| String::from("missing --transport argument"))?;
    }
    if flag != "--transport" {
        return Err(String::from("expected --transport argument"));
    }
    let path = args
        .next()
        .ok_or_else(|| String::from("missing transport path"))?;
    if args.next().is_some() {
        return Err(String::from("unexpected host arguments"));
    }
    Ok(path.into())
}

#[cfg(test)]
mod tests;
