use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr, eyre};
use kraai_agent::AgentManager;
use kraai_persistence::agent_state_root;
use kraai_provider_core::{ProviderManager, ProviderRegistry};
use kraai_provider_openai_chat_completions::{OpenAiChatCompletionsFactory, OpenAiFactory};
use kraai_provider_openai_codex::{
    OpenAiCodexAuthController, OpenAiCodexAuthControllerOptions, OpenAiCodexFactory,
};
use tokio::sync::{Mutex, RwLock, mpsc};

use super::core::{RuntimeConfig, RuntimeCore, emit_event};
use crate::api::Event;
use crate::api::RuntimeStartupState;
use crate::handle::{Command, RuntimeEventSender, RuntimeHandle, RuntimeLifecycle};

/// Builder for creating a runtime
pub struct RuntimeBuilder {
    mcp_config_path: Option<PathBuf>,
    provider_config_path: Option<PathBuf>,
    nushell_host_path: Option<PathBuf>,
    script_runtime_roots: Option<Vec<PathBuf>>,
    storage_root: Option<PathBuf>,
    use_current_executable_as_nushell_host: bool,
    resume_recovered_turns: bool,
}

struct RuntimeParts {
    handle: RuntimeHandle,
    lifecycle: Arc<RuntimeLifecycle>,
    event_tx: RuntimeEventSender,
    command_tx: mpsc::Sender<Command>,
    command_rx: mpsc::Receiver<Command>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    startup_tx: tokio::sync::watch::Sender<RuntimeStartupState>,
}

struct RuntimeHostOptions {
    mcp_config_path: Option<PathBuf>,
    nushell_host_path: Option<PathBuf>,
    script_runtime_roots: Option<Vec<PathBuf>>,
    storage_root: Option<PathBuf>,
    provider_config_path_override: Option<PathBuf>,
    use_current_executable_as_nushell_host: bool,
    initialize_tracing: bool,
    resume_recovered_turns: bool,
}

impl RuntimeParts {
    fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel(100);
        let event_tx = RuntimeEventSender::new(1024);
        let (startup_tx, startup_rx) = tokio::sync::watch::channel(RuntimeStartupState::Starting);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let lifecycle = Arc::new(RuntimeLifecycle::new(shutdown_tx));
        let handle = RuntimeHandle {
            command_tx: command_tx.clone(),
            event_tx: event_tx.clone(),
            lifecycle: Some(lifecycle.clone()),
            startup_rx,
        };
        Self {
            handle,
            lifecycle,
            event_tx,
            command_tx,
            command_rx,
            shutdown_rx,
            startup_tx,
        }
    }
}

impl RuntimeBuilder {
    /// Create a new runtime builder.
    pub fn new() -> Self {
        Self {
            mcp_config_path: None,
            provider_config_path: None,
            nushell_host_path: None,
            script_runtime_roots: None,
            storage_root: None,
            use_current_executable_as_nushell_host: false,
            resume_recovered_turns: true,
        }
    }

    pub fn storage_root(mut self, path: PathBuf) -> Self {
        self.storage_root = Some(path);
        self
    }

    pub fn mcp_config_path(mut self, path: PathBuf) -> Self {
        self.mcp_config_path = Some(path);
        self
    }

    pub fn script_runtime_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.script_runtime_roots = Some(roots);
        self
    }

    pub fn nushell_host_path(mut self, path: PathBuf) -> Self {
        self.nushell_host_path = Some(path);
        self
    }

    pub fn provider_config_path(mut self, path: PathBuf) -> Self {
        self.provider_config_path = Some(path);
        self
    }

    pub fn resume_recovered_turns(mut self, resume: bool) -> Self {
        self.resume_recovered_turns = resume;
        self
    }

    /// Uses this frontend executable as the sandboxed Nushell host unless an explicit host path
    /// is configured. The executable must call
    /// [`crate::run_internal_process`] before parsing its own arguments.
    pub fn use_current_executable_as_nushell_host(mut self) -> Self {
        self.use_current_executable_as_nushell_host = true;
        self
    }

    /// Build and start the runtime
    ///
    /// This spawns the runtime in a background thread and returns a handle
    /// to send commands.
    pub fn build(self) -> RuntimeHandle {
        let RuntimeParts {
            handle,
            lifecycle,
            event_tx,
            command_tx,
            command_rx,
            shutdown_rx,
            startup_tx,
        } = RuntimeParts::new();
        let host_options = RuntimeHostOptions {
            mcp_config_path: self.mcp_config_path,
            nushell_host_path: self.nushell_host_path,
            script_runtime_roots: self.script_runtime_roots,
            storage_root: self.storage_root,
            provider_config_path_override: self.provider_config_path,
            use_current_executable_as_nushell_host: self.use_current_executable_as_nushell_host,
            initialize_tracing: true,
            resume_recovered_turns: self.resume_recovered_turns,
        };
        let thread_startup_tx = startup_tx.clone();

        let thread = std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(error) => {
                    thread_startup_tx.send_replace(RuntimeStartupState::Failed(format!(
                        "Failed to create tokio runtime: {error}"
                    )));
                    emit_event(
                        &event_tx,
                        Event::ServiceError {
                            error: crate::RuntimeError::internal(format!(
                                "Failed to create tokio runtime: {error}"
                            )),
                        },
                    );
                    return;
                }
            };

            rt.block_on(Self::run_hosted(
                event_tx,
                command_tx,
                command_rx,
                shutdown_rx,
                thread_startup_tx,
                host_options,
            ));
        });
        lifecycle.set_thread(thread);

        handle
    }

    /// Build the runtime as a task on an existing Tokio runtime.
    ///
    /// This is the hosting mode for servers and other applications that already
    /// own their executor and tracing subscriber.
    pub fn build_on(self, runtime: &tokio::runtime::Handle) -> RuntimeHandle {
        let RuntimeParts {
            handle,
            lifecycle,
            event_tx,
            command_tx,
            command_rx,
            shutdown_rx,
            startup_tx,
        } = RuntimeParts::new();
        let host_options = RuntimeHostOptions {
            mcp_config_path: self.mcp_config_path,
            nushell_host_path: self.nushell_host_path,
            script_runtime_roots: self.script_runtime_roots,
            storage_root: self.storage_root,
            provider_config_path_override: self.provider_config_path,
            use_current_executable_as_nushell_host: self.use_current_executable_as_nushell_host,
            initialize_tracing: false,
            resume_recovered_turns: self.resume_recovered_turns,
        };
        let task_startup_tx = startup_tx.clone();
        let task = runtime.spawn(async move {
            Self::run_hosted(
                event_tx,
                command_tx,
                command_rx,
                shutdown_rx,
                task_startup_tx,
                host_options,
            )
            .await;
        });
        lifecycle.set_task(task);

        handle
    }

    async fn run_hosted(
        event_tx: RuntimeEventSender,
        command_tx: mpsc::Sender<Command>,
        command_rx: mpsc::Receiver<Command>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
        startup_tx: tokio::sync::watch::Sender<RuntimeStartupState>,
        host_options: RuntimeHostOptions,
    ) {
        if let Err(error) = Self::run_background(
            event_tx.clone(),
            command_tx,
            command_rx,
            shutdown_rx,
            startup_tx.clone(),
            host_options,
        )
        .await
        {
            let error = format!("{error:#}");
            startup_tx.send_replace(RuntimeStartupState::Failed(error.clone()));
            emit_event(
                &event_tx,
                Event::ServiceError {
                    error: crate::RuntimeError::internal(error),
                },
            );
        }
    }

    async fn run_background(
        event_tx: RuntimeEventSender,
        command_tx: mpsc::Sender<Command>,
        command_rx: mpsc::Receiver<Command>,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
        startup_tx: tokio::sync::watch::Sender<RuntimeStartupState>,
        host_options: RuntimeHostOptions,
    ) -> Result<()> {
        let storage_root = match host_options.storage_root {
            Some(path) => path,
            None => agent_state_root()?,
        };
        kraai_io::fs::create_dir_all_async(&storage_root)
            .await
            .wrap_err("Failed to initialize storage root")?;
        if host_options.initialize_tracing {
            Self::init_tracing(&storage_root)?;
        }
        let data_dir = storage_root.join("data");
        let auth_options = OpenAiCodexAuthControllerOptions::new(
            storage_root.clone(),
            storage_root.join("provider-state/openai-codex/auth.json"),
        );
        let (persistence, auth) = tokio::join!(
            kraai_persistence::Persistence::open(&data_dir),
            tokio::task::spawn_blocking(move || {
                OpenAiCodexAuthController::new_with_options(auth_options)
            }),
        );
        let persistence = persistence.wrap_err("Failed to initialize persistence layer")?;
        let execution_store = persistence.executions().clone();
        let context_state_store = persistence.context().clone();
        let image_store = persistence.images().clone();

        let providers = ProviderManager::new();
        let default_workspace_dir = std::env::current_dir()
            .and_then(|path| path.canonicalize())
            .or_else(|_| std::env::current_dir())
            .wrap_err("Failed to determine current workspace directory")?;
        let openai_codex_auth = Arc::new(
            auth.wrap_err("OpenAI auth initialization task failed")?
                .wrap_err("Failed to initialize OpenAI auth")?,
        );
        let registry = build_provider_registry(openai_codex_auth.clone())?;
        let provider_config_path = host_options
            .provider_config_path_override
            .unwrap_or_else(|| storage_root.join("providers.toml"));

        let agent_manager = Arc::new(RwLock::new(AgentManager::new(
            providers,
            default_workspace_dir,
            persistence,
            storage_root.clone(),
        )));

        let mcp_config_path = host_options
            .mcp_config_path
            .or_else(|| std::env::var_os("KRAAI_MCP_CONFIG").map(PathBuf::from))
            .unwrap_or_else(|| storage_root.join("mcp.toml"));
        let mcp_config = kraai_mcp::McpConfig::load(&mcp_config_path)
            .await
            .map_err(|error| eyre!(error))?;
        let mcp = kraai_mcp::McpManager::with_auth_storage(
            mcp_config,
            storage_root.clone(),
            storage_root.join("mcp-auth"),
        )
        .map_err(|error| eyre!(error))?;
        agent_manager.write().await.set_mcp(Arc::new(mcp));

        let runtime = RuntimeCore {
            image_store,
            queue_drains: Arc::default(),
            session_preparations: Arc::default(),
            event_tx,
            command_tx,
            agent_manager,
            execution_store,
            context_state_store,
            provider_registry: registry,
            active_streams: Arc::new(Mutex::new(std::collections::HashMap::new())),
            stream_tasks: Default::default(),
            active_script_tasks: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pending_script_approvals: Arc::new(Mutex::new(std::collections::HashMap::new())),
            queued_messages: Arc::new(Mutex::new(std::collections::HashMap::new())),
            session_state_barrier: Arc::new(RwLock::new(())),
            stopping: Arc::default(),
            openai_codex_auth,
            config: Arc::new(RuntimeConfig {
                provider_config_path,
                nushell_host_path: host_options.nushell_host_path,
                script_runtime_roots: host_options.script_runtime_roots,
                use_current_executable_as_nushell_host: host_options
                    .use_current_executable_as_nushell_host,
                resume_recovered_turns: host_options.resume_recovered_turns,
            }),
            startup_tx,
        };

        runtime.run(command_rx, shutdown_rx).await;
        Ok(())
    }

    fn init_tracing(storage_root: &std::path::Path) -> Result<()> {
        use color_eyre::eyre::Context;
        use std::sync::{Mutex, Once};

        static INIT: Once = Once::new();
        static TRACING_INIT_RESULT: Mutex<Option<Result<(), String>>> = Mutex::new(None);

        INIT.call_once(|| {
            let result = (|| -> Result<()> {
                let log_dir = storage_root.join("logs");

                std::fs::create_dir_all(&log_dir).wrap_err_with(|| {
                    format!("Failed to create log directory {}", log_dir.display())
                })?;

                let file_appender = tracing_appender::rolling::daily(&log_dir, "agent.log");

                let subscriber = tracing_subscriber::fmt()
                    .with_env_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                    )
                    .with_writer(file_appender)
                    .with_ansi(false)
                    .finish();

                tracing::subscriber::set_global_default(subscriber)
                    .map_err(|error| eyre!("Failed to set tracing subscriber: {error}"))?;
                Ok(())
            })();

            if let Ok(mut slot) = TRACING_INIT_RESULT.lock() {
                *slot = Some(result.map_err(|error| error.to_string()));
            }
        });

        TRACING_INIT_RESULT
            .lock()
            .map_err(|error| eyre!("Tracing init mutex poisoned: {error}"))?
            .as_ref()
            .map(|result| match result {
                Ok(()) => Ok(()),
                Err(error) => Err(eyre!(error.clone())),
            })
            .unwrap_or_else(|| Ok(()))
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn build_provider_registry(
    openai_codex_auth: Arc<OpenAiCodexAuthController>,
) -> Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::default();
    registry
        .register_factory::<OpenAiChatCompletionsFactory>()
        .map_err(|error| eyre!(error.to_string()))?;
    registry
        .register_factory::<OpenAiFactory>()
        .map_err(|error| eyre!(error.to_string()))?;
    let openai_codex_factory = OpenAiCodexFactory::new(openai_codex_auth);
    registry
        .register_dynamic_factory(
            OpenAiCodexFactory::TYPE_ID,
            OpenAiCodexFactory::definition(),
            OpenAiCodexFactory::pricing_policy(),
            move |id, config| {
                openai_codex_factory.create(id, config).map_err(|error| {
                    kraai_provider_core::ProviderError::ConfigParseError(error.to_string())
                })
            },
            OpenAiCodexFactory::validate_provider_config,
            OpenAiCodexFactory::validate_model_config,
        )
        .map_err(|error| eyre!(error.to_string()))?;
    Ok(registry)
}
