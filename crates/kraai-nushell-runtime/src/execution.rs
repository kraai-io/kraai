use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kraai_execution_types::{NushellStartup, SandboxCapabilities, ScriptExecutionId};
use kraai_sandbox::{ExecutionOutput, LaunchPlan, OutputEvent, PrivateTempConfig};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::effects::{RejectStateEffects, StateEffectHandler};
use crate::host_calls::{HostServices, serve};
use crate::images::{ImageAttachmentHandler, RejectImageAttachments};
use crate::rendering::Marker;
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
    pub startup_timeout: Duration,
    pub execution_started: Option<tokio::sync::oneshot::Receiver<tokio::time::Instant>>,
    pub active_commands: Vec<String>,
    pub nushell_startup: NushellStartup,
    pub output_events: Option<UnboundedSender<OutputEvent>>,
    pub private_temp: PrivateTempConfig,
    pub web_search: Arc<dyn kraai_web::WebSearch>,
    pub mcp: Arc<dyn kraai_mcp::McpHost>,
    pub state_effect_handler: Arc<dyn StateEffectHandler>,
    pub image_attachment_handler: Arc<dyn ImageAttachmentHandler>,
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
            startup_timeout: Duration::from_secs(5),
            execution_started: None,
            active_commands: Vec::new(),
            nushell_startup: NushellStartup::Clean,
            output_events: None,
            private_temp: PrivateTempConfig::default(),
            web_search: Arc::new(kraai_web::ExaSearch::default()),
            mcp: Arc::new(kraai_mcp::McpManager::default()),
            state_effect_handler: Arc::new(RejectStateEffects),
            image_attachment_handler: Arc::new(RejectImageAttachments),
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
    let timeout = plan.timeout;
    let startup_timeout = plan.startup_timeout;
    let secret = rand::random::<[u8; 32]>();
    let output_marker = Marker::random();
    let host_request = HostRequest {
        protocol_version: HOST_PROTOCOL_VERSION,
        execution_id: execution_id.clone(),
        source: plan.source,
        workspace_root: workspace_root.clone(),
        environment: plan.environment.clone(),
        active_commands: plan.active_commands,
        nushell_startup: plan.nushell_startup,
        event_secret: secret,
        output_marker: output_marker.clone(),
        restrict_network: !plan.capabilities.is_unsandboxed()
            && !plan
                .capabilities
                .contains(kraai_execution_types::SandboxCapability::Network),
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
    let (raw_output, raw_output_rx) = tokio::sync::mpsc::unbounded_channel();
    let (output_frames, output_frames_rx) = tokio::sync::mpsc::unbounded_channel();
    launch.output_events = Some(raw_output);
    let renderer = AbortOnDropHandle::new(tokio::spawn(crate::rendering::render(
        output_marker,
        output_frames_rx,
        raw_output_rx,
        plan.output_events,
    )));
    launch.private_temp = private_temp;
    launch.args(plan.host_arguments);
    listener.configure_launch(&mut launch);
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    launch.process_spawned = Some(spawned_tx);
    let (execution_tx, execution_rx) = tokio::sync::oneshot::channel();
    launch.execution_started = Some(execution_rx);
    let (timeout_started_tx, mut timeout_started) = tokio::sync::watch::channel(None);
    let requested_start = plan.execution_started;

    let channel_execution_id = execution_id.clone();
    let connected = Arc::new(AtomicBool::new(false));
    let connected_for_task = connected.clone();
    let mut channel_task = AbortOnDropHandle::new(tokio::spawn(async move {
        let transport = transport::accept(listener, spawned_rx, startup_timeout)
            .await
            .map_err(|error| ChannelError::Accept(error.to_string()))?;
        let connected_at = tokio::time::Instant::now();
        connected_for_task.store(true, Ordering::Release);
        let start_timeout = async move {
            let started = match requested_start {
                Some(requested) => requested
                    .await
                    .map_or(connected_at, |requested| requested.max(connected_at)),
                None => connected_at,
            };
            timeout_started_tx.send_replace(Some(started));
            let _ = execution_tx.send(started);
        };
        let session = async move {
            let (channel_reader, mut channel_writer) = tokio::io::split(transport);
            write_request(&mut channel_writer, &host_request)
                .await
                .map_err(|error| ChannelError::Request(error.to_string()))?;
            serve(
                tokio::io::BufReader::new(channel_reader),
                channel_writer,
                channel_execution_id,
                secret,
                HostServices {
                    effects: plan.state_effect_handler,
                    web: plan.web_search,
                    mcp: plan.mcp,
                    images: plan.image_attachment_handler,
                    output: output_frames,
                },
                &host_request.active_commands,
            )
            .await
            .map_err(|error| ChannelError::Host(error.to_string()))
        };
        tokio::pin!(start_timeout, session);
        let mut timeout_pending = true;
        loop {
            tokio::select! {
                biased;
                () = &mut start_timeout, if timeout_pending => timeout_pending = false,
                result = &mut session => return result,
            }
        }
    }));
    let host_cancellation = cancellation.child_token();
    let output = kraai_sandbox::run(launch, host_cancellation.clone());
    tokio::pin!(output);
    let (output, channel) = tokio::select! {
        biased;
        result = &mut channel_task => {
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
                if !connected.load(Ordering::Acquire) && !channel_task.is_finished() {
                    channel_task.abort();
                    Err(RuntimeError::Transport(String::from(
                        "Nushell host exited before connecting to the private transport",
                    )))
                } else {
                    let deadline = async {
                        let started = timeout_started
                            .wait_for(Option::is_some)
                            .await
                            .ok()
                            .and_then(|started| *started);
                        let Some(started) = started else {
                            return std::future::pending::<()>().await;
                        };
                        tokio::time::sleep_until(started + timeout).await;
                    };
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => {
                            channel_task.abort();
                            Ok(())
                        }
                        result = &mut channel_task => channel_result(result),
                        () = deadline => {
                            channel_task.abort();
                            output.termination = kraai_sandbox::Termination::TimedOut;
                            Ok(())
                        }
                    }
                }
            } else {
                channel_task.abort();
                Ok(())
            };
            (output.stdout, output.stderr) = renderer
                .await
                .map_err(|error| RuntimeError::ChannelTask(error.to_string()))?;
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
            channel_task.abort();
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
            ChannelError::Host(message) => RuntimeError::HostChannel(message),
        })
}

enum ChannelError {
    Accept(String),
    Request(String),
    Host(String),
}

#[derive(Debug)]
pub enum RuntimeError {
    Transport(String),
    RequestChannel(String),
    ChannelTask(String),
    HostChannel(String),
    Sandbox(kraai_sandbox::SandboxError),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "unable to create host transport: {message}"),
            Self::RequestChannel(message) => write!(f, "unable to send host request: {message}"),
            Self::ChannelTask(message) => write!(f, "host channel task failed: {message}"),
            Self::HostChannel(message) => write!(f, "host channel failed: {message}"),
            Self::Sandbox(error) => write!(f, "unable to execute Nushell host: {error}"),
        }
    }
}

impl std::error::Error for RuntimeError {}
