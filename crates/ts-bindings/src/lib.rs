#![deny(clippy::all)]

use std::sync::Arc;

use agent::AgentManager;
use color_eyre::eyre::{Context, Result, eyre};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use notify::{RecursiveMode, Watcher};
use provider_core::{ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use std::collections::{BTreeMap, HashMap};
use types::{MessageId, ModelId, ProviderId};

// ChatRole enum exposed to TypeScript
#[napi]
#[derive(Clone, Debug)]
pub enum ChatRole {
  System,
  User,
  Assistant,
  Tool,
}

impl From<types::ChatRole> for ChatRole {
  fn from(role: types::ChatRole) -> Self {
    match role {
      types::ChatRole::System => ChatRole::System,
      types::ChatRole::User => ChatRole::User,
      types::ChatRole::Assistant => ChatRole::Assistant,
      types::ChatRole::Tool => ChatRole::Tool,
    }
  }
}

// MessageStatus enum exposed to TypeScript
#[napi]
#[derive(Clone, Debug)]
pub enum MessageStatus {
  Complete,
  Streaming { call_id: String },
  Cancelled,
}

impl From<types::MessageStatus> for MessageStatus {
  fn from(status: types::MessageStatus) -> Self {
    match status {
      types::MessageStatus::Complete => MessageStatus::Complete,
      types::MessageStatus::Streaming { call_id } => MessageStatus::Streaming {
        call_id: call_id.to_string(),
      },
      types::MessageStatus::Cancelled => MessageStatus::Cancelled,
    }
  }
}

// Message struct exposed to TypeScript
#[napi(object)]
#[derive(Clone, Debug)]
pub struct Message {
  pub id: String,
  pub parent_id: Option<String>,
  pub role: ChatRole,
  pub content: String,
  pub status: MessageStatus,
}

impl From<types::Message> for Message {
  fn from(msg: types::Message) -> Self {
    Message {
      id: msg.id.to_string(),
      parent_id: msg.parent_id.map(|id| id.to_string()),
      role: msg.role.into(),
      content: msg.content,
      status: msg.status.into(),
    }
  }
}

// ChatMessage struct exposed to TypeScript (legacy compatibility)
#[napi(object)]
#[derive(Clone, Debug)]
pub struct ChatMessage {
  pub role: ChatRole,
  pub content: String,
}

impl From<types::ChatMessage> for ChatMessage {
  fn from(msg: types::ChatMessage) -> Self {
    ChatMessage {
      role: msg.role.into(),
      content: msg.content,
    }
  }
}

// Model struct exposed to TypeScript
#[napi(object)]
#[derive(Clone, Debug)]
pub struct Model {
  pub id: String,
  pub name: String,
}
use futures::StreamExt;
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use tokio::sync::{Mutex, mpsc, oneshot};
use tool_core::ToolManager;

fn to_napi_error(err: color_eyre::Report) -> napi::Error {
  napi::Error::new(Status::GenericFailure, format!("{:?}", err))
}

// Streaming events sent from Rust to TypeScript
#[napi]
#[derive(Clone)]
pub enum Event {
  ConfigLoaded,
  Error(String),
  MessageComplete(String),
  // Streaming events
  StreamStart { message_id: String },
  StreamChunk { message_id: String, chunk: String },
  StreamComplete { message_id: String },
  StreamError { message_id: String, error: String },
}

// Internal commands (not exposed to TypeScript)
enum Command {
  ListModels {
    response: oneshot::Sender<HashMap<String, Vec<Model>>>,
  },
  SendMessage {
    message: String,
    model_id: ModelId,
    provider_id: ProviderId,
  },
  LoadConfig,
  NewSession,
  GetChatHistory {
    response: oneshot::Sender<BTreeMap<MessageId, types::Message>>,
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
  pub async fn list_models(&self) -> napi::Result<HashMap<String, Vec<Model>>> {
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
        model_id: ModelId::new(model_id),
        provider_id: ProviderId::new(provider_id),
      })
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn get_chat_history_tree(&self) -> napi::Result<BTreeMap<String, Message>> {
    let (tx, rx) = oneshot::channel();

    self
      .send_command(Command::GetChatHistory { response: tx })
      .await
      .map_err(to_napi_error)?;

    let history: BTreeMap<MessageId, types::Message> =
      rx.await.map_err(|e| to_napi_error(e.into()))?;
    Ok(
      history
        .into_iter()
        .map(|(id, m)| (id.to_string(), m.into()))
        .collect(),
    )
  }

  #[napi]
  pub async fn new_session(&self) -> napi::Result<()> {
    self
      .send_command(Command::NewSession)
      .await
      .map_err(to_napi_error)
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
        let models_map = self.agent_manager.lock().await.list_models().await;
        let models: HashMap<String, Vec<Model>> = models_map
          .iter()
          .map(|(provider_id, model_list)| {
            let models: Vec<Model> = model_list
              .iter()
              .map(|m| Model {
                id: m.id.to_string(),
                name: m.name.clone(),
              })
              .collect();
            (provider_id.to_string(), models)
          })
          .collect();
        response.send(models).unwrap();
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
        let event_callback = self.event_callback.clone();
        let agent_manager = self.agent_manager.clone();

        tokio::spawn(async move {
          let (msg_id, mut stream) = {
            let mut agent = agent_manager.lock().await;
            match agent.start_stream(message, model_id, provider_id).await {
              Ok(result) => result,
              Err(e) => {
                event_callback.call(
                  Ok(Event::Error(e.to_string())),
                  ThreadsafeFunctionCallMode::NonBlocking,
                );
                return;
              }
            }
          };

          // Send stream start event
          event_callback.call(
            Ok(Event::StreamStart {
              message_id: msg_id.to_string(),
            }),
            ThreadsafeFunctionCallMode::NonBlocking,
          );

          // Process stream chunks
          while let Some(chunk_result) = stream.next().await {
            let chunk_result: Result<String, color_eyre::Report> = chunk_result;
            match chunk_result {
              Ok(chunk) => {
                // Append to message in session
                {
                  let agent = agent_manager.lock().await;
                  agent.append_chunk(&msg_id, &chunk).await;
                }

                // Send chunk event
                event_callback.call(
                  Ok(Event::StreamChunk {
                    message_id: msg_id.to_string(),
                    chunk,
                  }),
                  ThreadsafeFunctionCallMode::NonBlocking,
                );
              }
              Err(e) => {
                event_callback.call(
                  Ok(Event::StreamError {
                    message_id: msg_id.to_string(),
                    error: e.to_string(),
                  }),
                  ThreadsafeFunctionCallMode::NonBlocking,
                );
                return;
              }
            }
          }

          // Complete the message
          {
            let agent = agent_manager.lock().await;
            agent.complete_message(&msg_id).await;
          }

          // Send completion event
          event_callback.call(
            Ok(Event::StreamComplete {
              message_id: msg_id.to_string(),
            }),
            ThreadsafeFunctionCallMode::NonBlocking,
          );
        });
      }
      Command::NewSession => {
        self.agent_manager.lock().await.new_session();
      }
      Command::GetChatHistory { response } => {
        response
          .send(self.agent_manager.lock().await.get_chat_history().await)
          .unwrap();
      }
    };
    Ok(())
  }

  async fn spawn_config_watcher(&self) {
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

      for res in rx {
        match res {
          Ok(event) => {
            if event.kind.is_access() {
              continue;
            }
            if event.kind.is_remove() {
              watcher
                .watch(&config_loc, RecursiveMode::NonRecursive)
                .expect("failed to watch config file");
            }
            Self::send_command_tx(command_tx.clone(), Command::LoadConfig).await;
          }
          Err(e) => println!("[ERROR] config watch error: {:?}", e),
        }
      }
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
      .await?;

    Ok(())
  }
}
