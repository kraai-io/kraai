#![deny(clippy::all)]

use std::sync::Arc;
use std::time::Duration;

use agent::AgentManager;
use color_eyre::eyre::{Context, Result, eyre};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use provider_core::{
  ModelId, ProviderId, ProviderManager, ProviderManagerConfig, ProviderManagerHelper,
};
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use tokio::sync::{Mutex, mpsc, oneshot};
use tool_core::ToolManager;
use types::ChatMessage;

fn to_napi_error(err: color_eyre::Report) -> napi::Error {
  napi::Error::new(Status::GenericFailure, format!("{:?}", err))
}

// Example event sent from Rust to TypeScript
#[napi]
#[derive(Clone)]
pub enum Event {
  ConfigLoaded,
  Error(String),
}

// Internal commands (not exposed to TypeScript)
enum Command {
  ListModels {
    response: oneshot::Sender<Vec<String>>,
  },
  SendMessage {
    message: String,
    model_id: ModelId,
    provider_id: ProviderId,
  },
  LoadConfig,
}

// The runtime - owns a background tokio task with AgentManager
#[napi]
pub struct AgentRuntime {
  command_tx: mpsc::Sender<Command>,
  event_callback: Arc<ThreadsafeFunction<Event>>,
}

#[napi]
impl AgentRuntime {
  #[napi(constructor)]
  pub fn new(event_callback: ThreadsafeFunction<Event>) -> napi::Result<Self> {
    let (command_tx, command_rx) = mpsc::channel(100);
    let event_callback = Arc::new(event_callback);
    let callback_clone = event_callback.clone();
    let command_tx_clone = command_tx.clone();

    // Spawn background runtime in a new thread with its own tokio runtime
    std::thread::spawn(move || {
      let rt = tokio::runtime::Runtime::new().unwrap();
      rt.block_on(async {
        let providers = ProviderManager::new();
        let tools = ToolManager::default();
        let agent_manager = Arc::new(Mutex::new(AgentManager::new(providers, tools)));
        let runtime = Runtime {
          event_callback: callback_clone,
          command_tx: command_tx_clone,
          agent_manager,
        };
        runtime.runtime_loop(command_rx).await;
      });
    });

    Ok(AgentRuntime {
      command_tx,
      event_callback,
    })
  }

  // List available models from the AgentManager
  #[napi]
  pub async fn list_models(&self) -> napi::Result<Vec<String>> {
    let (tx, rx) = oneshot::channel();

    self
      .send_command(Command::ListModels { response: tx })
      .await
      .map_err(to_napi_error)?;

    rx.await.map_err(|e| to_napi_error(e.into()))
  }

  #[napi]
  pub async fn send_message(
    &self,
    message: String,
    model_id: String,
    provider_id: String,
  ) -> napi::Result<()> {
    self
      .send_command(Command::SendMessage {
        message,
        model_id: model_id.into(),
        provider_id: provider_id.into(),
      })
      .await
      .map_err(to_napi_error)?;
    Ok(())
  }

  async fn send_command(&self, command: Command) -> Result<()> {
    self.command_tx.send(command).await?;
    Ok(())
  }
}

struct Runtime {
  command_tx: mpsc::Sender<Command>,
  event_callback: Arc<ThreadsafeFunction<Event>>,
  agent_manager: Arc<Mutex<AgentManager>>,
}

impl Runtime {
  fn send_event(&self, event: Event) {
    self
      .event_callback
      .call(Ok(event), ThreadsafeFunctionCallMode::NonBlocking);
  }

  fn send_error_event(&self, error: impl Into<String>) {
    self.send_event(Event::Error(error.into()));
  }

  async fn send_command(&self, command: Command) {
    self
      .command_tx
      .send(command)
      .await
      .expect("failed to send command");
  }

  async fn send_command_tx(command_tx: mpsc::Sender<Command>, command: Command) {
    command_tx
      .send(command)
      .await
      .expect("failed to send command");
  }

  // Background event loop with AgentManager
  async fn runtime_loop(self, mut command_rx: mpsc::Receiver<Command>) {
    println!("[RUNTIME] Starting event loop");

    self.spawn_config_watcher().await;
    self.send_command(Command::LoadConfig).await;

    loop {
      let res = match command_rx.recv().await {
        Some(c) => self.handle_command(c).await,
        None => break,
      };
      if res.is_err() {
        self.send_error_event(res.err().unwrap().to_string());
      }
    }

    println!("[RUNTIME] Event loop terminated");
  }

  async fn handle_command(&self, command: Command) -> Result<()> {
    match command {
      Command::ListModels { response } => {
        response
          .send(
            self
              .agent_manager
              .lock()
              .await
              .list_models()
              .iter()
              .map(|x| x.name.clone())
              .collect(),
          )
          .unwrap();
      }
      Command::LoadConfig => {
        self.load_providers_config().await?;
        println!("[RUNTIME] loaded config");
        self.send_event(Event::ConfigLoaded);
      }
      Command::SendMessage {
        message,
        model_id,
        provider_id,
      } => {
        let res = self
          .agent_manager
          .lock()
          .await
          .send_message(message, model_id, provider_id)
          .await?;
        // should send an event
      }
    };
    Ok(())
  }

  async fn spawn_config_watcher(&self) {
    // TODO figure out why the config watcher does nothing after the first modification
    let command_tx = self.command_tx.clone();
    tokio::spawn(async move {
      let command_tx = command_tx.clone();
      let config_loc = directories::BaseDirs::new()
        .expect("Failed to find user directories")
        .home_dir()
        .join(".agent-desktop/providers.toml");
      let (tx, rx) = std::sync::mpsc::channel();
      let mut watcher = notify::recommended_watcher(tx).expect("failed to create config watcher");

      watcher
        .watch(&config_loc, RecursiveMode::NonRecursive)
        .expect("failed to watch config file");

      println!("[RUNTIME] started config watcher");
      for res in rx {
        match res {
          Ok(event) => {
            println!("[RUNTIME] config changed {:?}", event);
            Self::send_command_tx(command_tx.clone(), Command::LoadConfig).await;
          }
          Err(e) => println!("[ERROR] config watch error: {:?}", e),
        }
      }
      println!("[RUNTIME] ended config watcher");
    });
  }

  async fn load_providers_config(&self) -> Result<()> {
    let config_loc = directories::BaseDirs::new()
      .expect("Failed to find user directories")
      .home_dir()
      .join(".agent-desktop/providers.toml");

    if !config_loc.exists() {
      return Err(eyre!("Config file doesn't exist at {:?}", config_loc));
    }

    let config_slice = tokio::fs::read(config_loc).await?;
    let config: ProviderManagerConfig =
      toml::from_slice(&config_slice).wrap_err("Failed to parse providers.toml")?;

    let mut helper = ProviderManagerHelper::default();
    helper.register_factory::<GoogleFactory>();
    helper.register_factory::<OpenAIFactory>();

    self
      .agent_manager
      .lock()
      .await
      .set_providers(config, helper)
      .await
      .wrap_err("Failed to load providers from config")?;

    Ok(())
  }
}
