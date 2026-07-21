use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kraai_sandbox::{ExecutionOutput, LaunchPlan, OutputEvent, PrivateTempConfig};
use kraai_types::{NushellStartup, SandboxCapabilities, ScriptExecutionId};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::effects::{RejectStateEffects, StateEffectHandler, serve_effects};
use crate::request::{HOST_PROTOCOL_VERSION, HostRequest};
use crate::wire::{TRANSPORT_DESCRIPTOR, write_request};

pub struct ScriptExecutionPlan {
    pub execution_id: ScriptExecutionId,
    pub host_executable: PathBuf,
    pub source: Vec<u8>,
    pub workspace_root: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub runtime_roots: Vec<PathBuf>,
    pub capabilities: SandboxCapabilities,
    pub timeout: Duration,
    pub active_commands: Vec<String>,
    pub nushell_startup: NushellStartup,
    pub output_events: Option<UnboundedSender<OutputEvent>>,
    pub private_temp: PrivateTempConfig,
    pub state_effect_handler: Arc<dyn StateEffectHandler>,
}

impl ScriptExecutionPlan {
    pub fn new(
        execution_id: ScriptExecutionId,
        host_executable: PathBuf,
        source: Vec<u8>,
        workspace_root: PathBuf,
        capabilities: SandboxCapabilities,
        timeout: Duration,
    ) -> Self {
        Self {
            execution_id,
            host_executable,
            source,
            workspace_root,
            environment: BTreeMap::new(),
            runtime_roots: Vec::new(),
            capabilities,
            timeout,
            active_commands: Vec::new(),
            nushell_startup: NushellStartup::Clean,
            output_events: None,
            private_temp: PrivateTempConfig::default(),
            state_effect_handler: Arc::new(RejectStateEffects),
        }
    }
}

#[derive(Debug)]
pub struct ScriptExecutionResult {
    pub execution_id: ScriptExecutionId,
    pub output: ExecutionOutput,
}

pub async fn execute(
    plan: ScriptExecutionPlan,
    cancellation: CancellationToken,
) -> Result<ScriptExecutionResult, RuntimeError> {
    let execution_id = plan.execution_id.clone();
    let secret = rand::random::<[u8; 32]>();
    let host_request = HostRequest {
        protocol_version: HOST_PROTOCOL_VERSION,
        execution_id: execution_id.clone(),
        source: plan.source,
        workspace_root: plan.workspace_root.clone(),
        environment: plan.environment.clone(),
        active_commands: plan.active_commands,
        nushell_startup: plan.nushell_startup,
        event_secret: secret,
    };

    let private_temp = plan.private_temp.reserve().map_err(RuntimeError::Sandbox)?;
    let transport_path = private_temp
        .path()
        .ok_or_else(|| RuntimeError::Transport(String::from("private temp was not reserved")))?
        .join("host.sock");
    let listener = std::os::unix::net::UnixListener::bind(&transport_path)
        .map_err(|error| RuntimeError::Transport(error.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| RuntimeError::Transport(error.to_string()))?;
    let listener = tokio::net::UnixListener::from_std(listener)
        .map_err(|error| RuntimeError::Transport(error.to_string()))?;

    let mut launch = LaunchPlan::new(
        plan.host_executable,
        plan.workspace_root,
        plan.capabilities,
        plan.timeout,
    );
    launch.environment = plan
        .environment
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    launch.runtime_roots = plan.runtime_roots;
    launch.output_events = plan.output_events;
    launch.private_temp = private_temp;
    launch.arg("--transport").arg(&transport_path);
    launch
        .private_ipc_connect_descriptors
        .push(TRANSPORT_DESCRIPTOR);

    let effect_execution_id = execution_id.clone();
    let transport_connected = Arc::new(AtomicBool::new(false));
    let connected_for_task = transport_connected.clone();
    let effect_task = tokio::spawn(async move {
        let (transport, _) = listener
            .accept()
            .await
            .map_err(|error| ChannelError::Accept(error.to_string()))?;
        connected_for_task.store(true, Ordering::Release);
        let (effect_reader, mut effect_writer) = transport.into_split();
        write_request(&mut effect_writer, &host_request)
            .await
            .map_err(|error| ChannelError::Request(error.to_string()))?;
        serve_effects(
            effect_reader,
            effect_writer,
            effect_execution_id,
            secret,
            plan.state_effect_handler,
        )
        .await
        .map_err(|error| ChannelError::Effects(error.to_string()))
    });
    let output = kraai_sandbox::run(launch, cancellation).await;

    match output {
        Ok(output) => {
            if matches!(
                output.termination,
                kraai_sandbox::Termination::Exited { .. }
            ) {
                if !transport_connected.load(Ordering::Acquire) {
                    effect_task.abort();
                    return Err(RuntimeError::Transport(String::from(
                        "Nushell host exited before connecting to the private transport",
                    )));
                }
                match effect_task
                    .await
                    .map_err(|error| RuntimeError::ChannelTask(error.to_string()))?
                {
                    Ok(()) => {}
                    Err(ChannelError::Accept(message)) => {
                        return Err(RuntimeError::Transport(message));
                    }
                    Err(ChannelError::Request(message)) => {
                        return Err(RuntimeError::RequestChannel(message));
                    }
                    Err(ChannelError::Effects(message)) => {
                        return Err(RuntimeError::EffectChannel(message));
                    }
                }
            } else {
                effect_task.abort();
            }
            Ok(ScriptExecutionResult {
                execution_id,
                output,
            })
        }
        Err(error) => {
            effect_task.abort();
            Err(RuntimeError::Sandbox(error))
        }
    }
}

enum ChannelError {
    Accept(String),
    Request(String),
    Effects(String),
}

#[derive(Debug)]
pub enum RuntimeError {
    Transport(String),
    RequestChannel(String),
    ChannelTask(String),
    EffectChannel(String),
    Sandbox(kraai_sandbox::SandboxError),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "unable to create host transport: {message}"),
            Self::RequestChannel(message) => write!(f, "unable to send host request: {message}"),
            Self::ChannelTask(message) => write!(f, "host channel task failed: {message}"),
            Self::EffectChannel(message) => write!(f, "state effect channel failed: {message}"),
            Self::Sandbox(error) => write!(f, "unable to execute Nushell host: {error}"),
        }
    }
}

impl std::error::Error for RuntimeError {}
