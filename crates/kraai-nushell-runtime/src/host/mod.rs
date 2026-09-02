use kraai_command_core::{CommandRegistry, CommandRegistryError};
use kraai_types::NushellStartup;
use nu_protocol::PipelineData;
use nu_protocol::engine::{EngineState, Stack, StateWorkingSet};

use crate::request::{HOST_PROTOCOL_VERSION, HostRequest};

pub fn run_request(request: HostRequest, registry: &CommandRegistry) -> Result<i32, HostError> {
    validate_request(&request)?;
    std::env::set_current_dir(&request.workspace_root).map_err(|error| {
        HostError::Initialization(format!(
            "unable to enter workspace '{}': {error}",
            request.workspace_root.display()
        ))
    })?;

    let commands = registry
        .select(&request.active_commands)
        .map_err(HostError::Commands)?;
    let mut engine_state = build_engine(&request, commands)?;
    let mut stack = Stack::new();
    load_startup_files(&request, &mut engine_state, &mut stack);
    Ok(nu_cli::eval_source(
        &mut engine_state,
        &mut stack,
        &request.source,
        "kraai-script.nu",
        PipelineData::empty(),
        false,
    ))
}

fn load_startup_files(request: &HostRequest, engine_state: &mut EngineState, stack: &mut Stack) {
    if request.nushell_startup != NushellStartup::Inherit {
        return;
    }
    if !engine_state.config_dirs.is_resolved() {
        return;
    }
    let env_file = engine_state.config_dirs.env_file.to_path_buf();
    let config_file = engine_state.config_dirs.config_file.to_path_buf();
    nu_cli::eval_config_contents(env_file, engine_state, stack, false);
    nu_cli::eval_config_contents(config_file, engine_state, stack, false);
}

fn validate_request(request: &HostRequest) -> Result<(), HostError> {
    if request.protocol_version != HOST_PROTOCOL_VERSION {
        return Err(HostError::ProtocolVersion {
            expected: HOST_PROTOCOL_VERSION,
            received: request.protocol_version,
        });
    }
    if !request.workspace_root.is_absolute() || !request.workspace_root.is_dir() {
        return Err(HostError::Initialization(format!(
            "workspace '{}' is not an existing absolute directory",
            request.workspace_root.display()
        )));
    }
    Ok(())
}

fn build_engine(
    request: &HostRequest,
    commands: Vec<Box<dyn nu_protocol::engine::Command>>,
) -> Result<EngineState, HostError> {
    let mut engine_state = nu_cli::add_cli_context(nu_command::add_shell_command_context(
        nu_cmd_lang::create_default_context(),
    ));
    if let Ok((config_dirs, _warnings)) =
        nu_config::resolve_paths(&nu_config::SystemEnv, &nu_config::CliOverrides::default())
    {
        engine_state.config_dirs = config_dirs;
    }
    nu_cli::gather_parent_env_vars(&mut engine_state, &request.workspace_root);

    let mut working_set = StateWorkingSet::new(&engine_state);
    for command in commands {
        let name = command.name().as_bytes();
        if working_set.find_decl(name).is_some() {
            return Err(HostError::CommandShadowsBuiltin(command.name().to_owned()));
        }
        working_set.add_decl(command);
    }
    let delta = working_set.render();
    engine_state
        .merge_delta(delta)
        .map_err(|error| HostError::Initialization(error.to_string()))?;
    Ok(engine_state)
}

#[derive(Debug)]
pub enum HostError {
    ProtocolVersion { expected: u32, received: u32 },
    Initialization(String),
    Commands(CommandRegistryError),
    CommandShadowsBuiltin(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProtocolVersion { expected, received } => write!(
                f,
                "unsupported host protocol version {received}; expected {expected}"
            ),
            Self::Initialization(message) => write!(f, "host initialization failed: {message}"),
            Self::Commands(error) => write!(f, "invalid active command set: {error}"),
            Self::CommandShadowsBuiltin(name) => {
                write!(f, "Kraai command '{name}' shadows a Nushell built-in")
            }
        }
    }
}

impl std::error::Error for HostError {}
