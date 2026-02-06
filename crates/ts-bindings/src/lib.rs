#![deny(clippy::all)]

use std::sync::Arc;
use std::time::Duration;

use agent::AgentManager;
use color_eyre::eyre::{Context, Result};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use notify::{Config, Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};
use provider_core::{ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use tokio::sync::{mpsc, oneshot};
use tool_core::ToolManager;

fn to_napi_error(err: color_eyre::Report) -> napi::Error {
  napi::Error::new(Status::GenericFailure, format!("{:?}", err))
}

// Example event sent from Rust to TypeScript
#[napi(object)]
#[derive(Clone)]
pub struct Event {
  pub event_type: String,
  pub data: Option<String>,
}

// Internal commands (not exposed to TypeScript)
enum Command {
  ListModels {
    response: oneshot::Sender<Vec<String>>,
  },
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

    // Spawn background runtime in a new thread with its own tokio runtime
    std::thread::spawn(move || {
      let rt = tokio::runtime::Runtime::new().unwrap();
      rt.block_on(async {
        runtime_loop(command_rx, callback_clone).await;
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
      .command_tx
      .send(Command::ListModels { response: tx })
      .await
      .map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string()))?;

    rx.await
      .map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string()))
  }

  // Fire-and-forget that sends events back via callback
  #[napi]
  pub async fn do_something(&self) -> napi::Result<()> {
    let event = Event {
      event_type: "test".to_string(),
      data: Some("Hello from Rust!".to_string()),
    };

    self
      .event_callback
      .call(Ok(event), ThreadsafeFunctionCallMode::NonBlocking);

    Ok(())
  }
}

// Background event loop with AgentManager
async fn runtime_loop(
  mut command_rx: mpsc::Receiver<Command>,
  event_callback: Arc<ThreadsafeFunction<Event>>,
) {
  println!("[RUNTIME] Starting event loop");

  // Initialize AgentManager
  let providers = ProviderManager::new();
  let tools = ToolManager::default();
  let mut agent_manager = AgentManager::new(providers, tools);

  // Get config path
  let config_path = directories::BaseDirs::new()
    .expect("Failed to find user directories")
    .home_dir()
    .join(".agent-desktop/providers.toml");

  // Load config automatically on startup
  println!("[RUNTIME] Loading config...");
  match load_config_inner(&mut agent_manager).await {
    Ok(_) => {
      println!("[RUNTIME] Config loaded successfully");
      // Send config loaded event
      let event = Event {
        event_type: "config_loaded".to_string(),
        data: Some("Config loaded successfully".to_string()),
      };
      event_callback.call(Ok(event), ThreadsafeFunctionCallMode::NonBlocking);
    }
    Err(e) => {
      eprintln!("[RUNTIME] Failed to load config: {:?}", e);
      // Send config error event
      let event = Event {
        event_type: "config_error".to_string(),
        data: Some(format!("Failed to load config: {}", e)),
      };
      event_callback.call(Ok(event), ThreadsafeFunctionCallMode::NonBlocking);
    }
  }

  // Set up file watcher for config
  let (file_change_tx, mut file_change_rx) = mpsc::channel(10);
  let config_path_clone = config_path.clone();
  
  let _watcher_thread = std::thread::spawn(move || {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
      let (tx, mut rx) = mpsc::channel(10);
      
      let mut watcher = RecommendedWatcher::new(
        move |res: Result<NotifyEvent, notify::Error>| {
          let _ = tx.try_send(res);
        },
        Config::default().with_poll_interval(Duration::from_secs(1)),
      ).expect("Failed to create file watcher");

      watcher.watch(config_path_clone.parent().unwrap(), RecursiveMode::NonRecursive)
        .expect("Failed to watch config directory");

      println!("[RUNTIME] Config file watcher started");

      while let Some(res) = rx.recv().await {
        if let Ok(event) = res {
          if event.kind.is_modify() || event.kind.is_create() {
            for path in event.paths {
              if path.file_name().map(|n| n == "providers.toml").unwrap_or(false) {
                let _ = file_change_tx.try_send(());
              }
            }
          }
        }
      }
    });
  });

  println!("[RUNTIME] Event loop ready");

  loop {
    tokio::select! {
      // Handle commands from TypeScript
      cmd = command_rx.recv() => {
        match cmd {
          Some(Command::ListModels { response }) => {
            let models: Vec<String> = agent_manager
              .list_models()
              .into_iter()
              .map(|m| m.id.to_string())
              .collect();
            let _ = response.send(models);
          }
          None => {
            println!("[RUNTIME] Command channel closed, shutting down");
            break;
          }
        }
      }
      // Handle file change events
      _ = file_change_rx.recv() => {
        println!("[RUNTIME] Config file changed, reloading...");
        match load_config_inner(&mut agent_manager).await {
          Ok(_) => {
            println!("[RUNTIME] Config reloaded successfully");
            let event = Event {
              event_type: "config_reloaded".to_string(),
              data: Some("Config reloaded successfully".to_string()),
            };
            event_callback.call(Ok(event), ThreadsafeFunctionCallMode::NonBlocking);
          }
          Err(e) => {
            eprintln!("[RUNTIME] Failed to reload config: {:?}", e);
            let event = Event {
              event_type: "config_reload_error".to_string(),
              data: Some(format!("Failed to reload config: {}", e)),
            };
            event_callback.call(Ok(event), ThreadsafeFunctionCallMode::NonBlocking);
          }
        }
      }
    }
  }

  println!("[RUNTIME] Event loop terminated");
}

async fn load_config_inner(agent_manager: &mut AgentManager) -> Result<()> {
  let config_loc = directories::BaseDirs::new()
    .expect("Failed to find user directories")
    .home_dir()
    .join(".agent-desktop/providers.toml");

  if !config_loc.exists() {
    return Err(color_eyre::eyre::eyre!(
      "Config file doesn't exist at {:?}",
      config_loc
    ));
  }

  let config_slice = tokio::fs::read(config_loc).await?;
  let config: ProviderManagerConfig =
    toml::from_slice(&config_slice).wrap_err("Failed to parse providers.toml")?;

  let mut helper = ProviderManagerHelper::default();
  helper.register_factory::<GoogleFactory>();
  helper.register_factory::<OpenAIFactory>();

  agent_manager
    .set_providers(config, helper)
    .await
    .wrap_err("Failed to load providers from config")?;

  Ok(())
}
