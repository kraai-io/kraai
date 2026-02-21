#![deny(clippy::all)]

use std::sync::Arc;

use agent::AgentManager;
use color_eyre::eyre::{Context, Result, eyre};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;
use notify::{RecursiveMode, Watcher};
use persistence::SessionMeta;
use provider_core::{ProviderManager, ProviderManagerConfig, ProviderManagerHelper};
use std::collections::{BTreeMap, HashMap};
use tool_core::ToolManager;
use tool_read_file::ReadFileTool;
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
  ProcessingTools,
  Cancelled,
}

impl From<types::MessageStatus> for MessageStatus {
  fn from(status: types::MessageStatus) -> Self {
    match status {
      types::MessageStatus::Complete => MessageStatus::Complete,
      types::MessageStatus::Streaming { call_id } => MessageStatus::Streaming {
        call_id: call_id.to_string(),
      },
      types::MessageStatus::ProcessingTools => MessageStatus::ProcessingTools,
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

// SessionMeta struct exposed to TypeScript
#[napi(object)]
#[derive(Clone, Debug)]
pub struct Session {
  pub id: String,
  pub tip_id: Option<String>,
  pub created_at: f64,
  pub updated_at: f64,
  pub title: Option<String>,
}

impl From<SessionMeta> for Session {
  fn from(meta: SessionMeta) -> Self {
    Session {
      id: meta.id,
      tip_id: meta.tip_id.map(|id| id.to_string()),
      created_at: meta.created_at as f64,
      updated_at: meta.updated_at as f64,
      title: meta.title,
    }
  }
}
use futures::StreamExt;
use provider_google::GoogleFactory;
use provider_openai::OpenAIFactory;
use tokio::sync::{Mutex, mpsc, oneshot};

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
  StreamStart {
    message_id: String,
  },
  StreamChunk {
    message_id: String,
    chunk: String,
  },
  StreamComplete {
    message_id: String,
  },
  StreamError {
    message_id: String,
    error: String,
  },
  // Tool events
  ToolCallDetected {
    call_id: String,
    tool_id: String,
    args: String,
    description: String,
  },
  ToolResultReady {
    call_id: String,
    tool_id: String,
    success: bool,
    output: String,
    denied: bool,
  },
  // History events
  HistoryUpdated,
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
  ClearCurrentSession,
  LoadSession {
    session_id: String,
    response: oneshot::Sender<bool>,
  },
  ListSessions {
    response: oneshot::Sender<Vec<Session>>,
  },
  DeleteSession {
    session_id: String,
  },
  GetCurrentSessionId {
    response: oneshot::Sender<Option<String>>,
  },
  GetChatHistory {
    response: oneshot::Sender<BTreeMap<MessageId, types::Message>>,
  },
  ApproveTool {
    call_id: String,
  },
  DenyTool {
    call_id: String,
  },
  ExecuteApprovedTools,
}

// The runtime - owns a background tokio task with AgentManager
#[napi]
#[allow(dead_code)]
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
        // Initialize persistence layer
        let (message_store, session_store) = persistence::init()
          .await
          .expect("Failed to initialize persistence layer");

        let providers = ProviderManager::new();
        let mut tools = ToolManager::new();
        tools.register_tool(ReadFileTool {});
        let agent_manager = Arc::new(Mutex::new(AgentManager::new(
          providers,
          tools,
          message_store,
          session_store,
        )));
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
  pub async fn clear_current_session(&self) -> napi::Result<()> {
    self
      .send_command(Command::ClearCurrentSession)
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn load_session(&self, session_id: String) -> napi::Result<bool> {
    let (tx, rx) = oneshot::channel();

    self
      .send_command(Command::LoadSession {
        session_id,
        response: tx,
      })
      .await
      .map_err(to_napi_error)?;

    rx.await.map_err(|e| to_napi_error(e.into()))
  }

  #[napi]
  pub async fn list_sessions(&self) -> napi::Result<Vec<Session>> {
    let (tx, rx) = oneshot::channel();

    self
      .send_command(Command::ListSessions { response: tx })
      .await
      .map_err(to_napi_error)?;

    rx.await.map_err(|e| to_napi_error(e.into()))
  }

  #[napi]
  pub async fn delete_session(&self, session_id: String) -> napi::Result<()> {
    self
      .send_command(Command::DeleteSession { session_id })
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn get_current_session_id(&self) -> napi::Result<Option<String>> {
    let (tx, rx) = oneshot::channel();

    self
      .send_command(Command::GetCurrentSessionId { response: tx })
      .await
      .map_err(to_napi_error)?;

    rx.await.map_err(|e| to_napi_error(e.into()))
  }

  #[napi]
  pub async fn approve_tool(&self, call_id: String) -> napi::Result<()> {
    self
      .send_command(Command::ApproveTool { call_id })
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn deny_tool(&self, call_id: String) -> napi::Result<()> {
    self
      .send_command(Command::DenyTool { call_id })
      .await
      .map_err(to_napi_error)
  }

  #[napi]
  pub async fn execute_approved_tools(&self) -> napi::Result<()> {
    self
      .send_command(Command::ExecuteApprovedTools)
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

          let mut full_content = String::new();

          // Process stream chunks
          while let Some(chunk_result) = stream.next().await {
            let chunk_result: Result<String, color_eyre::Report> = chunk_result;
            match chunk_result {
              Ok(chunk) => {
                full_content.push_str(&chunk);

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

          println!("[RUNTIME] Full content length: {}", full_content.len());
          if full_content.contains("tool_call") {
            println!("[RUNTIME] Content contains tool_call marker");
          }

          // Complete the message
          {
            let agent = agent_manager.lock().await;
            let _ = agent.complete_message(&msg_id).await;
          }

          // Send completion event
          event_callback.call(
            Ok(Event::StreamComplete {
              message_id: msg_id.to_string(),
            }),
            ThreadsafeFunctionCallMode::NonBlocking,
          );

          // Notify UI that history was updated (new session may have been created)
          event_callback.call(
            Ok(Event::HistoryUpdated),
            ThreadsafeFunctionCallMode::NonBlocking,
          );

          // Check for tool calls in the completed message
          let (tool_calls, failed) = {
            let mut agent = agent_manager.lock().await;
            agent.parse_tool_calls_from_content(&full_content).await
          };

          println!(
            "[RUNTIME] Found {} tool calls, {} failed",
            tool_calls.len(),
            failed.len()
          );

          // Add failed tool calls to history and reprompt
          if !failed.is_empty() {
            println!("[RUNTIME] Failed tool calls found, adding to history and reprompting");
            let mut agent = agent_manager.lock().await;
            if let Err(e) = agent.add_failed_tool_calls_to_history(failed).await {
              println!("[RUNTIME] Error adding failed tool calls to history: {}", e);
            }
            event_callback.call(
              Ok(Event::HistoryUpdated),
              ThreadsafeFunctionCallMode::NonBlocking,
            );

            // Start continuation stream to let model retry
            println!("[RUNTIME] Starting continuation stream for retry");
            let continuation = {
              let mut agent = agent_manager.lock().await;
              agent.start_continuation_stream().await
            };

            match continuation {
              Ok(Some((msg_id, mut stream))) => {
                println!("[RUNTIME] Continuation stream started: {}", msg_id);
                event_callback.call(
                  Ok(Event::StreamStart {
                    message_id: msg_id.to_string(),
                  }),
                  ThreadsafeFunctionCallMode::NonBlocking,
                );

                let mut cont_content = String::new();
                while let Some(chunk_result) = stream.next().await {
                  let chunk_result: Result<String, color_eyre::Report> = chunk_result;
                  match chunk_result {
                    Ok(chunk) => {
                      cont_content.push_str(&chunk);
                      {
                        let agent = agent_manager.lock().await;
                        agent.append_chunk(&msg_id, &chunk).await;
                      }
                      event_callback.call(
                        Ok(Event::StreamChunk {
                          message_id: msg_id.to_string(),
                          chunk,
                        }),
                        ThreadsafeFunctionCallMode::NonBlocking,
                      );
                    }
                    Err(e) => {
                      println!("[RUNTIME] Continuation stream error: {}", e);
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

                {
                  let agent = agent_manager.lock().await;
                  let _ = agent.complete_message(&msg_id).await;
                }

                println!("[RUNTIME] Continuation stream completed");
                event_callback.call(
                  Ok(Event::StreamComplete {
                    message_id: msg_id.to_string(),
                  }),
                  ThreadsafeFunctionCallMode::NonBlocking,
                );

                // Recursively check for more tool calls
                let (more_calls, more_failed) = {
                  let mut agent = agent_manager.lock().await;
                  agent.parse_tool_calls_from_content(&cont_content).await
                };

                println!(
                  "[RUNTIME] After retry: {} tool calls, {} failed",
                  more_calls.len(),
                  more_failed.len()
                );

                for (call_id, tool_id, description) in more_calls {
                  let args_json = {
                    let agent = agent_manager.lock().await;
                    let pending = agent.get_pending_tool_args(&call_id);
                    pending
                      .map(|a| serde_json::to_string(&a).unwrap_or_default())
                      .unwrap_or_default()
                  };
                  event_callback.call(
                    Ok(Event::ToolCallDetected {
                      call_id: call_id.to_string(),
                      tool_id,
                      args: args_json,
                      description,
                    }),
                    ThreadsafeFunctionCallMode::NonBlocking,
                  );
                }

                if !more_failed.is_empty() {
                  let mut agent = agent_manager.lock().await;
                  let _ = agent.add_failed_tool_calls_to_history(more_failed).await;
                  event_callback.call(
                    Ok(Event::HistoryUpdated),
                    ThreadsafeFunctionCallMode::NonBlocking,
                  );
                }
              }
              Ok(None) => {
                println!("[RUNTIME] No continuation stream available (no model/provider set)");
              }
              Err(e) => {
                println!("[RUNTIME] Failed to start continuation stream: {}", e);
              }
            }
            return;
          }

          // Emit tool call detected events
          for (call_id, tool_id, description) in tool_calls {
            let args_json = {
              let agent = agent_manager.lock().await;
              let pending = agent.get_pending_tool_args(&call_id);
              pending
                .map(|a| serde_json::to_string(&a).unwrap_or_default())
                .unwrap_or_default()
            };

            println!(
              "[RUNTIME] Emitting ToolCallDetected: {} - {}",
              tool_id, description
            );

            event_callback.call(
              Ok(Event::ToolCallDetected {
                call_id: call_id.to_string(),
                tool_id,
                args: args_json,
                description,
              }),
              ThreadsafeFunctionCallMode::NonBlocking,
            );
          }
        });
      }
      Command::ClearCurrentSession => {
        self.agent_manager.lock().await.clear_current_session().await;
      }
      Command::LoadSession {
        session_id,
        response,
      } => {
        let loaded = self.agent_manager.lock().await.load_session(&session_id).await?;
        response.send(loaded).unwrap();
      }
      Command::ListSessions { response } => {
        let sessions = self.agent_manager.lock().await.list_sessions().await?;
        let sessions: Vec<Session> = sessions.into_iter().map(|s| s.into()).collect();
        response.send(sessions).unwrap();
      }
      Command::DeleteSession { session_id } => {
        self.agent_manager.lock().await.delete_session(&session_id).await?;
      }
      Command::GetCurrentSessionId { response } => {
        let session_id = self.agent_manager.lock().await.get_current_session_id().map(|s| s.to_string());
        response.send(session_id).unwrap();
      }
      Command::GetChatHistory { response } => {
        response
          .send(self.agent_manager.lock().await.get_chat_history().await?)
          .unwrap();
      }
      Command::ApproveTool { call_id } => {
        let call_id = types::CallId::new(call_id);
        self.agent_manager.lock().await.approve_tool(call_id);
      }
      Command::DenyTool { call_id } => {
        let call_id = types::CallId::new(call_id);
        self.agent_manager.lock().await.deny_tool(call_id);
      }
      Command::ExecuteApprovedTools => {
        let event_callback = self.event_callback.clone();
        let agent_manager = self.agent_manager.clone();
        let command_tx = self.command_tx.clone();

        tokio::spawn(async move {
          let results = {
            let mut agent = agent_manager.lock().await;
            agent.execute_approved_tools().await
          };

          for result in &results {
            let success = !result.output.get("error").is_some_and(|e| e.is_string());
            let output = serde_json::to_string(&result.output).unwrap_or_default();

            event_callback.call(
              Ok(Event::ToolResultReady {
                call_id: result.call_id.to_string(),
                tool_id: result.tool_id.to_string(),
                success,
                output,
                denied: result.permission_denied,
              }),
              ThreadsafeFunctionCallMode::NonBlocking,
            );
          }

          // Add results to history
          {
            let mut agent = agent_manager.lock().await;
            let _ = agent.add_tool_results_to_history(results).await;
          }
          println!("[RUNTIME] Emitting HistoryUpdated event after tool results");
          event_callback.call(
            Ok(Event::HistoryUpdated),
            ThreadsafeFunctionCallMode::NonBlocking,
          );

          // Start continuation stream
          let continuation = {
            let mut agent = agent_manager.lock().await;
            agent.start_continuation_stream().await
          };

          if let Ok(Some((msg_id, mut stream))) = continuation {
            event_callback.call(
              Ok(Event::StreamStart {
                message_id: msg_id.to_string(),
              }),
              ThreadsafeFunctionCallMode::NonBlocking,
            );

            while let Some(chunk_result) = stream.next().await {
              let chunk_result: Result<String, color_eyre::Report> = chunk_result;
              match chunk_result {
                Ok(chunk) => {
                  {
                    let agent = agent_manager.lock().await;
                    agent.append_chunk(&msg_id, &chunk).await;
                  }
                  event_callback.call(
                    Ok(Event::StreamChunk {
                      message_id: msg_id.to_string(),
                      chunk,
                    }),
                    ThreadsafeFunctionCallMode::NonBlocking,
                  );
                }
                Err(e) => {
                  println!("[RUNTIME] Continuation stream error: {}", e);
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

            {
              let agent = agent_manager.lock().await;
              let _ = agent.complete_message(&msg_id).await;
            }

            event_callback.call(
              Ok(Event::StreamComplete {
                message_id: msg_id.to_string(),
              }),
              ThreadsafeFunctionCallMode::NonBlocking,
            );

            // Check for more tool calls
            let (tool_calls, failed) = {
              let mut agent = agent_manager.lock().await;
              match agent.get_chat_history().await {
                Ok(history) => {
                  if let Some(msg) = history.get(&msg_id) {
                    agent.parse_tool_calls_from_content(&msg.content).await
                  } else {
                    (vec![], vec![])
                  }
                }
                Err(e) => {
                  println!("[RUNTIME] Error getting chat history: {}", e);
                  (vec![], vec![])
                }
              }
            };

            // Add failed tool calls to history and reprompt
            if !failed.is_empty() {
              let mut agent = agent_manager.lock().await;
              if let Err(e) = agent.add_failed_tool_calls_to_history(failed).await {
                println!("[RUNTIME] Error adding failed tool calls to history: {}", e);
              }
              event_callback.call(
                Ok(Event::HistoryUpdated),
                ThreadsafeFunctionCallMode::NonBlocking,
              );

              // Trigger another continuation for the model to retry
              if let Ok(Some((retry_id, mut retry_stream))) = {
                let mut agent = agent_manager.lock().await;
                agent.start_continuation_stream().await
              } {
                event_callback.call(
                  Ok(Event::StreamStart {
                    message_id: retry_id.to_string(),
                  }),
                  ThreadsafeFunctionCallMode::NonBlocking,
                );

                while let Some(chunk_result) = retry_stream.next().await {
                  if let Ok(chunk) = chunk_result {
                    {
                      let agent = agent_manager.lock().await;
                      agent.append_chunk(&retry_id, &chunk).await;
                    }
                    event_callback.call(
                      Ok(Event::StreamChunk {
                        message_id: retry_id.to_string(),
                        chunk,
                      }),
                      ThreadsafeFunctionCallMode::NonBlocking,
                    );
                  }
                }

                {
                  let agent = agent_manager.lock().await;
                  let _ = agent.complete_message(&retry_id).await;
                }
                event_callback.call(
                  Ok(Event::StreamComplete {
                    message_id: retry_id.to_string(),
                  }),
                  ThreadsafeFunctionCallMode::NonBlocking,
                );
              }
            }

            for (call_id, tool_id, description) in tool_calls {
              let args_json = {
                let agent = agent_manager.lock().await;
                let pending = agent.get_pending_tool_args(&call_id);
                pending
                  .map(|a| serde_json::to_string(&a).unwrap_or_default())
                  .unwrap_or_default()
              };

              event_callback.call(
                Ok(Event::ToolCallDetected {
                  call_id: call_id.to_string(),
                  tool_id,
                  args: args_json,
                  description,
                }),
                ThreadsafeFunctionCallMode::NonBlocking,
              );
            }
          }

          let _ = command_tx;
        });
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
    helper
      .register_factory::<GoogleFactory>()
      .map_err(|e| eyre!("{}", e))?;
    helper
      .register_factory::<OpenAIFactory>()
      .map_err(|e| eyre!("{}", e))?;

    self
      .agent_manager
      .lock()
      .await
      .set_providers(config, helper)
      .await?;

    Ok(())
  }
}
