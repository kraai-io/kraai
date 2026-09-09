#![deny(unsafe_code)]

mod effects;
mod execution;
pub mod host;
pub mod request;
mod wire;

pub use effects::{RejectStateEffects, StateEffectHandler};
pub use execution::{RuntimeError, ScriptExecutionPlan, ScriptExecutionResult, execute};

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
    let transport = match wire::connect_transport(&transport_path) {
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
    let effect_client = std::sync::Arc::new(effects::DescriptorEffectClient::from_transport(
        request.execution_id.clone(),
        request.event_secret,
        transport,
    ));
    let context = kraai_command_core::CommandContext::new(effect_client);
    let registry = match kraai_command_catalog::command_registry(context) {
        Ok(registry) => registry,
        Err(error) => {
            report_host_error(format!("invalid built-in command registry: {error}"));
            return 70;
        }
    };
    match host::run_request(request, &registry) {
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
mod tests {
    use super::*;

    #[test]
    fn host_transport_accepts_standalone_and_internal_invocations() {
        let standalone = ["--transport", "/tmp/host.sock"]
            .into_iter()
            .map(std::ffi::OsString::from);
        assert_eq!(
            host_transport_path_from(standalone),
            Ok(std::path::PathBuf::from("/tmp/host.sock"))
        );

        let internal = [
            INTERNAL_HOST_ARGUMENT,
            "--transport",
            "/tmp/internal-host.sock",
        ]
        .into_iter()
        .map(std::ffi::OsString::from);
        assert_eq!(
            host_transport_path_from(internal),
            Ok(std::path::PathBuf::from("/tmp/internal-host.sock"))
        );
    }
}
