use crossbeam_channel::{Receiver, Sender, unbounded};
use kraai_runtime::{CreateSessionRequest, RuntimeError, RuntimeEvent, RuntimeHandle};
use tokio::sync::broadcast;

use super::auth::map_openai_codex_auth_status;
use super::{RuntimeRequest, RuntimeResponse};

#[derive(Debug)]
pub(super) enum RuntimeEventBridgeMessage {
    Event(RuntimeEvent),
    Lagged(u64),
    StartupComplete(kraai_runtime::RuntimeResult<kraai_runtime::RuntimeStartupState>),
}

pub(super) fn spawn_event_bridge(runtime: RuntimeHandle) -> Receiver<RuntimeEventBridgeMessage> {
    let runtime_events = runtime.subscribe();
    let (event_tx, event_rx) = unbounded();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(error) => {
                let _ = event_tx.send(RuntimeEventBridgeMessage::StartupComplete(Err(
                    RuntimeError::unavailable(format!("failed to create tokio runtime: {error}")),
                )));
                return;
            }
        };
        rt.block_on(forward_runtime_events(
            runtime_events,
            event_tx,
            async move { runtime.wait_for_startup().await },
        ));
    });

    event_rx
}

async fn forward_runtime_events(
    mut runtime_events: broadcast::Receiver<RuntimeEvent>,
    event_tx: Sender<RuntimeEventBridgeMessage>,
    startup: impl Future<Output = kraai_runtime::RuntimeResult<kraai_runtime::RuntimeStartupState>>,
) {
    let result = {
        tokio::pin!(startup);
        loop {
            tokio::select! {
                result = &mut startup => break result,
                event = runtime_events.recv() => {
                    if !forward_runtime_event(&event_tx, event) {
                        break Err(RuntimeError::unavailable("runtime event channel is closed"));
                    }
                }
            }
        }
    };
    if !drain_startup_events(&mut runtime_events, |event| {
        forward_runtime_event(&event_tx, event)
    }) {
        return;
    }
    if event_tx
        .send(RuntimeEventBridgeMessage::StartupComplete(result))
        .is_err()
    {
        return;
    }
    loop {
        if !forward_runtime_event(&event_tx, runtime_events.recv().await) {
            return;
        }
    }
}

fn drain_startup_events(
    runtime_events: &mut broadcast::Receiver<RuntimeEvent>,
    mut forward: impl FnMut(Result<RuntimeEvent, broadcast::error::RecvError>) -> bool,
) -> bool {
    let mut remaining = runtime_events.len();
    while remaining > 0 {
        let event = match runtime_events.try_recv() {
            Ok(event) => {
                remaining -= 1;
                Ok(event)
            }
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                remaining =
                    remaining.saturating_sub(usize::try_from(skipped).unwrap_or(usize::MAX));
                Err(broadcast::error::RecvError::Lagged(skipped))
            }
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                break;
            }
        };
        if !forward(event) {
            return false;
        }
    }
    true
}

fn forward_runtime_event(
    sender: &Sender<RuntimeEventBridgeMessage>,
    event: Result<RuntimeEvent, broadcast::error::RecvError>,
) -> bool {
    let message = match event {
        Ok(event) => RuntimeEventBridgeMessage::Event(event),
        Err(broadcast::error::RecvError::Lagged(skipped)) => {
            RuntimeEventBridgeMessage::Lagged(skipped)
        }
        Err(broadcast::error::RecvError::Closed) => return false,
    };
    sender.send(message).is_ok()
}

pub(super) fn spawn_runtime_bridge(
    runtime: RuntimeHandle,
) -> (Sender<RuntimeRequest>, Receiver<RuntimeResponse>) {
    let (runtime_tx, req_rx) = unbounded();
    let (res_tx, runtime_rx) = unbounded();

    std::thread::spawn(move || {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                RuntimeError::unavailable(format!("failed to create tokio runtime: {error}"))
            });
        let bridge = RequestBridge { runtime, executor };
        while let Ok(request) = req_rx.recv() {
            let _ = res_tx.send(bridge.dispatch(request));
        }
    });

    (runtime_tx, runtime_rx)
}

struct RequestBridge {
    executor: kraai_runtime::RuntimeResult<tokio::runtime::Runtime>,
    runtime: RuntimeHandle,
}

impl RequestBridge {
    fn execute<'a, T, F>(
        &'a self,
        request: impl FnOnce(&'a RuntimeHandle) -> F,
    ) -> kraai_runtime::RuntimeResult<T>
    where
        F: Future<Output = kraai_runtime::RuntimeResult<T>>,
    {
        match &self.executor {
            Ok(executor) => executor.block_on(request(&self.runtime)),
            Err(error) => Err(error.clone()),
        }
    }

    fn dispatch(&self, req: RuntimeRequest) -> RuntimeResponse {
        match req {
            RuntimeRequest::FinishStartupSync => RuntimeResponse::StartupSyncComplete,
            RuntimeRequest::ListModels => {
                let result = self.execute(|runtime| runtime.list_models());
                RuntimeResponse::Models(result)
            }
            RuntimeRequest::GetAgentProfileCatalog => {
                let result = self.execute(|runtime| runtime.get_agent_profile_catalog(None));
                RuntimeResponse::AgentProfileCatalog(result)
            }
            RuntimeRequest::ListProviderDefinitions => {
                let result = self.execute(|runtime| runtime.list_provider_definitions());
                RuntimeResponse::ProviderDefinitions(result)
            }
            RuntimeRequest::GetSettings => {
                let result = self.execute(|runtime| runtime.get_settings());
                RuntimeResponse::Settings(result)
            }
            RuntimeRequest::GetOpenAiCodexAuthStatus => {
                let result = self
                    .execute(|runtime| runtime.get_openai_codex_auth_status())
                    .map(map_openai_codex_auth_status);
                RuntimeResponse::OpenAiCodexAuthStatus(result)
            }
            RuntimeRequest::StartOpenAiCodexBrowserLogin => {
                let result = self
                    .execute(|runtime| runtime.start_openai_codex_browser_login())
                    .and_then(|_| {
                        self.execute(|runtime| runtime.get_openai_codex_auth_status())
                            .map(map_openai_codex_auth_status)
                    });
                RuntimeResponse::StartOpenAiCodexBrowserLogin(result)
            }
            RuntimeRequest::StartOpenAiCodexDeviceCodeLogin => {
                let result = self
                    .execute(|runtime| runtime.start_openai_codex_device_code_login())
                    .and_then(|_| {
                        self.execute(|runtime| runtime.get_openai_codex_auth_status())
                            .map(map_openai_codex_auth_status)
                    });
                RuntimeResponse::StartOpenAiCodexDeviceCodeLogin(result)
            }
            RuntimeRequest::CancelOpenAiCodexLogin => {
                let result = self
                    .execute(|runtime| runtime.cancel_openai_codex_login())
                    .and_then(|_| {
                        self.execute(|runtime| runtime.get_openai_codex_auth_status())
                            .map(map_openai_codex_auth_status)
                    });
                RuntimeResponse::CancelOpenAiCodexLogin(result)
            }
            RuntimeRequest::LogoutOpenAiCodexAuth => {
                let result = self
                    .execute(|runtime| runtime.logout_openai_codex_auth())
                    .and_then(|_| {
                        self.execute(|runtime| runtime.get_openai_codex_auth_status())
                            .map(map_openai_codex_auth_status)
                    });
                RuntimeResponse::LogoutOpenAiCodexAuth(result)
            }
            RuntimeRequest::SetSessionProfile {
                session_id,
                profile_id,
            } => {
                let result = self.execute(|runtime| {
                    runtime.set_session_profile(session_id.clone(), profile_id.clone())
                });
                RuntimeResponse::SetSessionProfile {
                    session_id,
                    profile_id,
                    result,
                }
            }
            RuntimeRequest::CreateSession {
                creation_id,
                profile_id,
            } => {
                let result = self.execute(|runtime| {
                    runtime.create_session_with(CreateSessionRequest {
                        workspace_dir: None,
                        profile_id,
                    })
                });
                RuntimeResponse::CreateSession {
                    creation_id,
                    result,
                }
            }
            RuntimeRequest::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
            } => {
                let result = self.execute(|runtime| {
                    runtime.send_content(session_id, message, model_id, provider_id)
                });
                RuntimeResponse::SendMessage(result)
            }
            RuntimeRequest::ImportImage { path, session_id } => {
                let origin_session_id = session_id.clone();
                let result = self.execute(|runtime| async move {
                    let path = if path.is_relative()
                        && let Some(session_id) = session_id
                    {
                        let workspace =
                            runtime
                                .get_workspace_state(session_id)
                                .await?
                                .ok_or_else(|| {
                                    RuntimeError::unavailable("Session workspace is unavailable")
                                })?;
                        std::path::Path::new(&workspace.workspace_dir).join(path)
                    } else {
                        path
                    };
                    let bytes = super::images::read_image_file(&path).await?;
                    runtime.import_image(bytes).await
                });
                RuntimeResponse::ImportImage {
                    session_id: origin_session_id,
                    result,
                }
            }
            RuntimeRequest::SaveSettings { settings } => {
                let result = self.execute(|runtime| runtime.save_settings(settings));
                RuntimeResponse::SaveSettings(result)
            }
            RuntimeRequest::GetChatHistory { session_id } => {
                let result = self.execute(|runtime| runtime.get_chat_history(session_id.clone()));
                RuntimeResponse::ChatHistory { session_id, result }
            }
            RuntimeRequest::GetSessionSnapshot { session_id } => {
                let result =
                    self.execute(|runtime| runtime.get_session_snapshot(session_id.clone()));
                RuntimeResponse::SessionSnapshot {
                    session_id,
                    result: Box::new(result),
                }
            }
            RuntimeRequest::GetCurrentTip { session_id } => {
                let result = self.execute(|runtime| runtime.get_tip(session_id.clone()));
                RuntimeResponse::CurrentTip { session_id, result }
            }
            RuntimeRequest::UndoLastUserMessage { session_id } => {
                let result =
                    self.execute(|runtime| runtime.undo_last_user_message(session_id.clone()));
                RuntimeResponse::UndoLastUserMessage { session_id, result }
            }
            RuntimeRequest::LoadSession {
                load_id,
                session_id,
            } => {
                let result = self.execute(|runtime| runtime.load_session(session_id.clone()));
                RuntimeResponse::LoadSession {
                    load_id,
                    session_id,
                    result,
                }
            }
            RuntimeRequest::ListSessions => {
                let result = self.execute(|runtime| runtime.list_sessions());
                RuntimeResponse::Sessions(result)
            }
            RuntimeRequest::ListUserInputHistory { limit } => {
                let result = self.execute(|runtime| runtime.list_user_input_history(limit));
                RuntimeResponse::UserInputHistory(result)
            }
            RuntimeRequest::DeleteSession { session_id } => {
                let result = self.execute(|runtime| runtime.delete_session(session_id.clone()));
                RuntimeResponse::DeleteSession { session_id, result }
            }
            RuntimeRequest::ApproveScript {
                session_id,
                execution_id,
            } => {
                let result = self.execute(|runtime| {
                    runtime.approve_script(session_id.clone(), execution_id.clone())
                });
                RuntimeResponse::ApproveScript {
                    session_id,
                    execution_id,
                    result,
                }
            }
            RuntimeRequest::DenyScript {
                session_id,
                execution_id,
            } => {
                let result = self.execute(|runtime| {
                    runtime.deny_script(session_id.clone(), execution_id.clone())
                });
                RuntimeResponse::DenyScript {
                    session_id,
                    execution_id,
                    result,
                }
            }
            RuntimeRequest::CancelStream { session_id } => {
                let result = self.execute(|runtime| runtime.cancel_stream(session_id));
                RuntimeResponse::CancelStream(result)
            }
            RuntimeRequest::ContinueSession { session_id } => {
                let result = self.execute(|runtime| runtime.continue_session(session_id));
                RuntimeResponse::ContinueSession(result)
            }
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "bridge tests propagate fixture errors and assert event ordering"
)]
#[path = "runtime_bridge_tests.rs"]
mod tests;
