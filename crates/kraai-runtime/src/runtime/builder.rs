use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr, eyre};
use kraai_agent::AgentManager;
use kraai_persistence::agent_state_root;
use kraai_provider_core::{ProviderManager, ProviderRegistry};
use kraai_provider_openai_chat_completions::{OpenAiChatCompletionsFactory, OpenAiFactory};
use kraai_provider_openai_codex::{OpenAiCodexAuthController, OpenAiCodexFactory};
use tokio::sync::{Mutex, RwLock, mpsc};

use super::core::{RuntimeCore, emit_event};
use crate::api::Event;
use crate::api::RuntimeStartupState;
use crate::handle::{Command, RuntimeEventSender, RuntimeHandle, RuntimeLifecycle};
use crate::settings::resolve_provider_config_path;

/// Builder for creating a runtime
pub struct RuntimeBuilder {
    provider_config_path: Option<PathBuf>,
    use_current_executable_as_nushell_host: bool,
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
    provider_config_path_override: Option<PathBuf>,
    use_current_executable_as_nushell_host: bool,
    initialize_tracing: bool,
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
            provider_config_path: None,
            use_current_executable_as_nushell_host: false,
        }
    }

    pub fn provider_config_path(mut self, path: PathBuf) -> Self {
        self.provider_config_path = Some(path);
        self
    }

    /// Allows this frontend executable to serve as the sandboxed Nushell host when a packaged
    /// sibling host binary is unavailable. The executable must call
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
            provider_config_path_override: self.provider_config_path,
            use_current_executable_as_nushell_host: self.use_current_executable_as_nushell_host,
            initialize_tracing: true,
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
            provider_config_path_override: self.provider_config_path,
            use_current_executable_as_nushell_host: self.use_current_executable_as_nushell_host,
            initialize_tracing: false,
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
        if host_options.initialize_tracing {
            Self::init_tracing()?;
        }

        let (message_store, session_store, execution_store, context_state_store) =
            kraai_persistence::init()
                .await
                .wrap_err("Failed to initialize persistence layer")?;

        let providers = ProviderManager::new();
        let default_workspace_dir = std::env::current_dir()
            .and_then(|path| path.canonicalize())
            .or_else(|_| std::env::current_dir())
            .wrap_err("Failed to determine current workspace directory")?;
        let openai_codex_auth = Arc::new(
            OpenAiCodexAuthController::new().wrap_err("Failed to initialize OpenAI auth")?,
        );
        let registry = build_provider_registry(openai_codex_auth.clone())?;
        let provider_config_path =
            resolve_provider_config_path(host_options.provider_config_path_override)?;

        let agent_manager = Arc::new(RwLock::new(AgentManager::new(
            providers,
            default_workspace_dir,
            message_store,
            session_store,
            context_state_store.clone(),
        )));

        let runtime = RuntimeCore {
            queue_drains: Arc::default(),
            session_preparations: Arc::default(),
            event_tx,
            command_tx,
            agent_manager,
            execution_store,
            context_state_store,
            provider_registry: registry,
            active_streams: Arc::new(Mutex::new(std::collections::HashMap::new())),
            active_script_tasks: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pending_script_approvals: Arc::new(Mutex::new(std::collections::HashMap::new())),
            queued_messages: Arc::new(Mutex::new(std::collections::HashMap::new())),
            session_state_barrier: Arc::new(RwLock::new(())),
            openai_codex_auth,
            provider_config_path,
            use_current_executable_as_nushell_host: host_options
                .use_current_executable_as_nushell_host,
            startup_tx,
        };

        runtime.run(command_rx, shutdown_rx).await;
        Ok(())
    }

    fn init_tracing() -> Result<()> {
        use color_eyre::eyre::Context;
        use std::sync::{Mutex, Once};

        static INIT: Once = Once::new();
        static TRACING_INIT_RESULT: Mutex<Option<Result<(), String>>> = Mutex::new(None);

        INIT.call_once(|| {
            let result = (|| -> Result<()> {
                let log_dir = agent_state_root()?.join("logs");

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
