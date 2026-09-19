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
use tokio_util::task::AbortOnDropHandle;

use crate::effects::{RejectStateEffects, StateEffectHandler, serve_effects};
use crate::request::{HOST_PROTOCOL_VERSION, HostRequest};
use crate::transport;
use crate::wire::write_request;

pub struct ScriptExecutionPlan {
    pub execution_id: ScriptExecutionId,
    pub host_executable: PathBuf,
    pub host_arguments: Vec<OsString>,
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
            host_arguments: Vec::new(),
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
    let workspace_root = plan.workspace_root.canonicalize().map_err(|error| {
        RuntimeError::Sandbox(kraai_sandbox::SandboxError::MissingWorkspace(format!(
            "unable to resolve '{}': {error}",
            plan.workspace_root.display()
        )))
    })?;
    let execution_id = plan.execution_id.clone();
    let secret = rand::random::<[u8; 32]>();
    let host_request = HostRequest {
        protocol_version: HOST_PROTOCOL_VERSION,
        execution_id: execution_id.clone(),
        source: plan.source,
        workspace_root: workspace_root.clone(),
        environment: plan.environment.clone(),
        active_commands: plan.active_commands,
        nushell_startup: plan.nushell_startup,
        event_secret: secret,
        restrict_network: !plan.capabilities.is_unsandboxed()
            && !plan
                .capabilities
                .contains(kraai_types::SandboxCapability::Network),
    };

    let private_temp = plan.private_temp.reserve();
    #[cfg(windows)]
    let private_temp = private_temp.map_err(|error| error.with_workspace_root(&workspace_root));
    let private_temp = private_temp.map_err(RuntimeError::Sandbox)?;
    let transport_directory = private_temp
        .path()
        .ok_or_else(|| RuntimeError::Transport(String::from("private temp was not reserved")))?;
    let mut listener = transport::Listener::bind(transport_directory)
        .map_err(|error| RuntimeError::Transport(error.to_string()))?;

    let mut launch = LaunchPlan::new(
        plan.host_executable,
        workspace_root,
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
    launch.args(plan.host_arguments);
    listener.configure_launch(&mut launch);
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    launch.process_spawned = Some(spawned_tx);
    let (execution_tx, execution_rx) = tokio::sync::oneshot::channel();
    launch.execution_started = Some(execution_rx);

    let effect_execution_id = execution_id.clone();
    let transport_connected = Arc::new(AtomicBool::new(false));
    let connected_for_task = transport_connected.clone();
    let mut effect_task = AbortOnDropHandle::new(tokio::spawn(async move {
        let transport = transport::accept(listener, spawned_rx)
            .await
            .map_err(|error| ChannelError::Accept(error.to_string()))?;
        connected_for_task.store(true, Ordering::Release);
        let _ = execution_tx.send(tokio::time::Instant::now());
        let (effect_reader, mut effect_writer) = tokio::io::split(transport);
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
    }));
    let host_cancellation = cancellation.child_token();
    let output = kraai_sandbox::run(launch, host_cancellation.clone());
    tokio::pin!(output);
    let (output, channel) = tokio::select! {
        biased;
        result = &mut effect_task => {
            let result = channel_result(result);
            if result.is_err() {
                host_cancellation.cancel();
            }
            (output.await, Some(result))
        }
        output = &mut output => (output, None),
    };

    match output {
        Ok(mut output) => {
            let channel = if let Some(channel) = channel {
                channel
            } else if matches!(
                output.termination,
                kraai_sandbox::Termination::Exited { .. }
            ) {
                if !transport_connected.load(Ordering::Acquire) && !effect_task.is_finished() {
                    effect_task.abort();
                    Err(RuntimeError::Transport(String::from(
                        "Nushell host exited before connecting to the private transport",
                    )))
                } else {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => {
                            effect_task.abort();
                            Ok(())
                        }
                        result = &mut effect_task => channel_result(result),
                    }
                }
            } else {
                effect_task.abort();
                Ok(())
            };
            if cancellation.is_cancelled() {
                output.termination = kraai_sandbox::Termination::Cancelled;
                output.sandbox_denied = false;
            } else if output.termination != kraai_sandbox::Termination::TimedOut {
                channel?;
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

fn channel_result(
    result: Result<Result<(), ChannelError>, tokio::task::JoinError>,
) -> Result<(), RuntimeError> {
    result
        .map_err(|error| RuntimeError::ChannelTask(error.to_string()))?
        .map_err(|error| match error {
            ChannelError::Accept(message) => RuntimeError::Transport(message),
            ChannelError::Request(message) => RuntimeError::RequestChannel(message),
            ChannelError::Effects(message) => RuntimeError::EffectChannel(message),
        })
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
