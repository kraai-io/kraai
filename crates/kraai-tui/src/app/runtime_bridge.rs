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
    let (runtime_tx, req_rx): (Sender<RuntimeRequest>, Receiver<RuntimeRequest>) = unbounded();
    let (res_tx, runtime_rx): (Sender<RuntimeResponse>, Receiver<RuntimeResponse>) = unbounded();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(error) => {
                let message = format!("failed to create tokio runtime: {error}");
                while let Ok(req) = req_rx.recv() {
                    respond_with_runtime_error(&res_tx, req, &message);
                }
                return;
            }
        };

        while let Ok(req) = req_rx.recv() {
            match req {
                RuntimeRequest::FinishStartupSync => {
                    let _ = res_tx.send(RuntimeResponse::StartupSyncComplete);
                }
                RuntimeRequest::ListModels => {
                    let result = rt.block_on(runtime.list_models());
                    let _ = res_tx.send(RuntimeResponse::Models(result));
                }
                RuntimeRequest::GetAgentProfileCatalog => {
                    let result = rt.block_on(runtime.get_agent_profile_catalog(None));
                    let _ = res_tx.send(RuntimeResponse::AgentProfileCatalog(result));
                }
                RuntimeRequest::ListProviderDefinitions => {
                    let result = rt.block_on(runtime.list_provider_definitions());
                    let _ = res_tx.send(RuntimeResponse::ProviderDefinitions(result));
                }
                RuntimeRequest::GetSettings => {
                    let result = rt.block_on(runtime.get_settings());
                    let _ = res_tx.send(RuntimeResponse::Settings(result));
                }
                RuntimeRequest::GetOpenAiCodexAuthStatus => {
                    let result = rt
                        .block_on(runtime.get_openai_codex_auth_status())
                        .map(map_openai_codex_auth_status);
                    let _ = res_tx.send(RuntimeResponse::OpenAiCodexAuthStatus(result));
                }
                RuntimeRequest::StartOpenAiCodexBrowserLogin => {
                    let result = rt
                        .block_on(runtime.start_openai_codex_browser_login())
                        .and_then(|_| {
                            rt.block_on(runtime.get_openai_codex_auth_status())
                                .map(map_openai_codex_auth_status)
                        });
                    let _ = res_tx.send(RuntimeResponse::StartOpenAiCodexBrowserLogin(result));
                }
                RuntimeRequest::StartOpenAiCodexDeviceCodeLogin => {
                    let result = rt
                        .block_on(runtime.start_openai_codex_device_code_login())
                        .and_then(|_| {
                            rt.block_on(runtime.get_openai_codex_auth_status())
                                .map(map_openai_codex_auth_status)
                        });
                    let _ = res_tx.send(RuntimeResponse::StartOpenAiCodexDeviceCodeLogin(result));
                }
                RuntimeRequest::CancelOpenAiCodexLogin => {
                    let result = rt
                        .block_on(runtime.cancel_openai_codex_login())
                        .and_then(|_| {
                            rt.block_on(runtime.get_openai_codex_auth_status())
                                .map(map_openai_codex_auth_status)
                        });
                    let _ = res_tx.send(RuntimeResponse::CancelOpenAiCodexLogin(result));
                }
                RuntimeRequest::LogoutOpenAiCodexAuth => {
                    let result = rt
                        .block_on(runtime.logout_openai_codex_auth())
                        .and_then(|_| {
                            rt.block_on(runtime.get_openai_codex_auth_status())
                                .map(map_openai_codex_auth_status)
                        });
                    let _ = res_tx.send(RuntimeResponse::LogoutOpenAiCodexAuth(result));
                }
                RuntimeRequest::SetSessionProfile {
                    session_id,
                    profile_id,
                } => {
                    let result = rt.block_on(
                        runtime.set_session_profile(session_id.clone(), profile_id.clone()),
                    );
                    let _ = res_tx.send(RuntimeResponse::SetSessionProfile {
                        session_id,
                        profile_id,
                        result,
                    });
                }
                RuntimeRequest::CreateSession {
                    creation_id,
                    profile_id,
                } => {
                    let result = rt.block_on(runtime.create_session_with(CreateSessionRequest {
                        workspace_dir: None,
                        profile_id,
                    }));
                    let _ = res_tx.send(RuntimeResponse::CreateSession {
                        creation_id,
                        result,
                    });
                }
                RuntimeRequest::SendMessage {
                    session_id,
                    message,
                    model_id,
                    provider_id,
                } => {
                    let result = rt.block_on(runtime.send_message(
                        session_id,
                        message,
                        model_id,
                        provider_id,
                    ));
                    let _ = res_tx.send(RuntimeResponse::SendMessage(result));
                }
                RuntimeRequest::SaveSettings { settings } => {
                    let result = rt.block_on(runtime.save_settings(settings));
                    let _ = res_tx.send(RuntimeResponse::SaveSettings(result));
                }
                RuntimeRequest::GetChatHistory { session_id } => {
                    let result = rt.block_on(runtime.get_chat_history(session_id.clone()));
                    let _ = res_tx.send(RuntimeResponse::ChatHistory { session_id, result });
                }
                RuntimeRequest::GetSessionSnapshot { session_id } => {
                    let result = rt.block_on(runtime.get_session_snapshot(session_id.clone()));
                    let _ = res_tx.send(RuntimeResponse::SessionSnapshot {
                        session_id,
                        result: Box::new(result),
                    });
                }
                RuntimeRequest::GetCurrentTip { session_id } => {
                    let result = rt.block_on(runtime.get_tip(session_id.clone()));
                    let _ = res_tx.send(RuntimeResponse::CurrentTip { session_id, result });
                }
                RuntimeRequest::UndoLastUserMessage { session_id } => {
                    let result = rt.block_on(runtime.undo_last_user_message(session_id.clone()));
                    let _ =
                        res_tx.send(RuntimeResponse::UndoLastUserMessage { session_id, result });
                }
                RuntimeRequest::LoadSession { session_id } => {
                    let result = rt.block_on(runtime.load_session(session_id.clone()));
                    let _ = res_tx.send(RuntimeResponse::LoadSession { session_id, result });
                }
                RuntimeRequest::ListSessions => {
                    let result = rt.block_on(runtime.list_sessions());
                    let _ = res_tx.send(RuntimeResponse::Sessions(result));
                }
                RuntimeRequest::ListUserInputHistory { limit } => {
                    let result = rt.block_on(runtime.list_user_input_history(limit));
                    let _ = res_tx.send(RuntimeResponse::UserInputHistory(result));
                }
                RuntimeRequest::DeleteSession { session_id } => {
                    let result = rt.block_on(runtime.delete_session(session_id.clone()));
                    let _ = res_tx.send(RuntimeResponse::DeleteSession { session_id, result });
                }
                RuntimeRequest::ApproveScript {
                    session_id,
                    execution_id,
                } => {
                    let result = rt
                        .block_on(runtime.approve_script(session_id.clone(), execution_id.clone()));
                    let _ = res_tx.send(RuntimeResponse::ApproveScript {
                        session_id,
                        execution_id,
                        result,
                    });
                }
                RuntimeRequest::DenyScript {
                    session_id,
                    execution_id,
                } => {
                    let result =
                        rt.block_on(runtime.deny_script(session_id.clone(), execution_id.clone()));
                    let _ = res_tx.send(RuntimeResponse::DenyScript {
                        session_id,
                        execution_id,
                        result,
                    });
                }
                RuntimeRequest::CancelStream { session_id } => {
                    let result = rt.block_on(runtime.cancel_stream(session_id));
                    let _ = res_tx.send(RuntimeResponse::CancelStream(result));
                }
                RuntimeRequest::ContinueSession { session_id } => {
                    let result = rt.block_on(runtime.continue_session(session_id));
                    let _ = res_tx.send(RuntimeResponse::ContinueSession(result));
                }
            }
        }
    });

    (runtime_tx, runtime_rx)
}

fn respond_with_runtime_error(
    res_tx: &Sender<RuntimeResponse>,
    req: RuntimeRequest,
    message: &str,
) {
    let error = RuntimeError::unavailable(message);
    let response = match req {
        RuntimeRequest::FinishStartupSync => RuntimeResponse::StartupSyncComplete,
        RuntimeRequest::ListModels => RuntimeResponse::Models(Err(error.clone())),
        RuntimeRequest::GetAgentProfileCatalog => {
            RuntimeResponse::AgentProfileCatalog(Err(error.clone()))
        }
        RuntimeRequest::ListProviderDefinitions => {
            RuntimeResponse::ProviderDefinitions(Err(error.clone()))
        }
        RuntimeRequest::GetSettings => RuntimeResponse::Settings(Err(error.clone())),
        RuntimeRequest::GetOpenAiCodexAuthStatus => {
            RuntimeResponse::OpenAiCodexAuthStatus(Err(error.clone()))
        }
        RuntimeRequest::StartOpenAiCodexBrowserLogin => {
            RuntimeResponse::StartOpenAiCodexBrowserLogin(Err(error.clone()))
        }
        RuntimeRequest::StartOpenAiCodexDeviceCodeLogin => {
            RuntimeResponse::StartOpenAiCodexDeviceCodeLogin(Err(error.clone()))
        }
        RuntimeRequest::CancelOpenAiCodexLogin => {
            RuntimeResponse::CancelOpenAiCodexLogin(Err(error.clone()))
        }
        RuntimeRequest::LogoutOpenAiCodexAuth => {
            RuntimeResponse::LogoutOpenAiCodexAuth(Err(error.clone()))
        }
        RuntimeRequest::SetSessionProfile {
            session_id,
            profile_id,
        } => RuntimeResponse::SetSessionProfile {
            session_id,
            profile_id,
            result: Err(error.clone()),
        },
        RuntimeRequest::CreateSession { creation_id, .. } => RuntimeResponse::CreateSession {
            creation_id,
            result: Err(error.clone()),
        },
        RuntimeRequest::SendMessage { .. } => RuntimeResponse::SendMessage(Err(error.clone())),
        RuntimeRequest::SaveSettings { .. } => RuntimeResponse::SaveSettings(Err(error.clone())),
        RuntimeRequest::GetChatHistory { session_id } => RuntimeResponse::ChatHistory {
            session_id,
            result: Err(error.clone()),
        },
        RuntimeRequest::GetSessionSnapshot { session_id } => RuntimeResponse::SessionSnapshot {
            session_id,
            result: Box::new(Err(error.clone())),
        },
        RuntimeRequest::GetCurrentTip { session_id } => RuntimeResponse::CurrentTip {
            session_id,
            result: Err(error.clone()),
        },
        RuntimeRequest::UndoLastUserMessage { session_id } => {
            RuntimeResponse::UndoLastUserMessage {
                session_id,
                result: Err(error.clone()),
            }
        }
        RuntimeRequest::LoadSession { session_id } => RuntimeResponse::LoadSession {
            session_id,
            result: Err(error.clone()),
        },
        RuntimeRequest::ListSessions => RuntimeResponse::Sessions(Err(error.clone())),
        RuntimeRequest::ListUserInputHistory { .. } => {
            RuntimeResponse::UserInputHistory(Err(error.clone()))
        }
        RuntimeRequest::DeleteSession { session_id } => RuntimeResponse::DeleteSession {
            session_id,
            result: Err(error.clone()),
        },
        RuntimeRequest::ApproveScript {
            session_id,
            execution_id,
        } => RuntimeResponse::ApproveScript {
            session_id,
            execution_id,
            result: Err(error.clone()),
        },
        RuntimeRequest::DenyScript {
            session_id,
            execution_id,
        } => RuntimeResponse::DenyScript {
            session_id,
            execution_id,
            result: Err(error.clone()),
        },
        RuntimeRequest::CancelStream { .. } => RuntimeResponse::CancelStream(Err(error.clone())),
        RuntimeRequest::ContinueSession { .. } => RuntimeResponse::ContinueSession(Err(error)),
    };

    let _ = res_tx.send(response);
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "bridge tests propagate fixture errors and assert event ordering"
)]
#[path = "runtime_bridge_tests.rs"]
mod tests;
