#![deny(unsafe_code)]

mod effects;
mod execution;
pub mod host;
pub mod request;
mod wire;

pub use effects::{RejectStateEffects, StateEffectHandler};
pub use execution::{RuntimeError, ScriptExecutionPlan, ScriptExecutionResult, execute};

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
    let registry = match production_command_registry(context) {
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
    let mut args = std::env::args_os();
    let _executable = args.next();
    let flag = args
        .next()
        .ok_or_else(|| String::from("missing --transport argument"))?;
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

fn production_command_registry(
    context: kraai_command_core::CommandContext,
) -> Result<kraai_command_core::CommandRegistry, kraai_command_core::CommandRegistryError> {
    let open_files = kraai_command_open_files::OpenFilesCommand::registration(context.clone())?;
    let close_files = kraai_command_close_files::CloseFilesCommand::registration(context.clone())?;
    let edit_file = kraai_command_edit_file::EditFileCommand::registration(context)?;
    kraai_command_core::CommandRegistry::new([open_files, close_files, edit_file])
}
