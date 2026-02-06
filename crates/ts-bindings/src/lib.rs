#![deny(clippy::all)]

use std::sync::Arc;

use agent::AgentManager;
use color_eyre::eyre::Result;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use tokio::sync::Mutex;

// Example event sent from Rust to TypeScript
#[napi(object)]
#[derive(Clone)]
pub struct Event {
  pub event_type: String,
  pub data: Option<String>,
}

// The runtime - owns a background tokio task
#[napi]
pub struct AgentRuntime {
  // Channel to send commands to the background task
  command_tx: tokio::sync::mpsc::Sender<Command>,
  // Event callback to send data back to TypeScript
  event_callback: Arc<ThreadsafeFunction<Event>>,
  agent_manager: AgentManager,
}

// Internal commands (not exposed to TypeScript)
enum Command {
  ListModels {
    response: tokio::sync::oneshot::Sender<Vec<String>>,
  },
}

#[napi]
impl AgentRuntime {
  #[napi(constructor)]
  pub fn new(event_callback: ThreadsafeFunction<Event>) -> napi::Result<Self> {
    let (command_tx, command_rx) = tokio::sync::mpsc::channel(100);
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

  // Example: async method that returns data
  #[napi]
  pub async fn list_models(&self) -> napi::Result<Vec<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    self
      .command_tx
      .send(Command::ListModels { response: tx })
      .await
      .map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string()))?;

    rx.await
      .map_err(|e| napi::Error::new(Status::GenericFailure, e.to_string()))
  }

  // Example: fire-and-forget that sends events back via callback
  #[napi]
  pub async fn do_something(&self) -> napi::Result<()> {
    // Send an event back to TypeScript
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

// Background event loop
async fn runtime_loop(
  mut command_rx: tokio::sync::mpsc::Receiver<Command>,
  event_callback: Arc<ThreadsafeFunction<Event>>,
) {
  println!("[RUNTIME] Event loop started");

  while let Some(cmd) = command_rx.recv().await {
    match cmd {
      Command::ListModels { response } => {
        // Example: return some data
        let models = vec!["model-1".to_string(), "model-2".to_string()];
        let _ = response.send(models);
      }
    }
  }

  println!("[RUNTIME] Event loop terminated");
}
