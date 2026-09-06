use crossbeam_channel::{Receiver, Sender, unbounded};
use kraai_runtime::{
    CreateSessionRequest, OpenAiCodexAuthStatus as RuntimeOpenAiCodexAuthStatus,
    OpenAiCodexLoginState as RuntimeOpenAiCodexLoginState, RuntimeError, RuntimeEvent,
    RuntimeHandle,
};
use tokio::sync::broadcast;

use super::{ProviderAuthState, ProviderAuthStatus, RuntimeRequest, RuntimeResponse};

#[derive(Debug)]
pub(super) enum RuntimeEventBridgeMessage {
    Event(RuntimeEvent),
    Lagged(u64),
}

pub(super) fn spawn_event_bridge(
    mut runtime_events: broadcast::Receiver<RuntimeEvent>,
) -> Receiver<RuntimeEventBridgeMessage> {
    let (event_tx, event_rx) = unbounded();

    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Runtime::new() else {
            return;
        };

        rt.block_on(async move {
            loop {
                match runtime_events.recv().await {
                    Ok(event) => {
                        if event_tx
                            .send(RuntimeEventBridgeMessage::Event(event))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        if event_tx
                            .send(RuntimeEventBridgeMessage::Lagged(skipped))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    });

    event_rx
}

pub(super) fn spawn_runtime_bridge(
    runtime: RuntimeHandle,
) -> (Sender<RuntimeRequest>, Receiver<RuntimeResponse>) {
    let (runtime_tx, req_rx): (Sender<RuntimeRequest>, Receiver<RuntimeRequest>) = unbounded();
    let (res_tx, runtime_rx): (Sender<RuntimeResponse>, Receiver<RuntimeResponse>) = unbounded();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
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

fn map_openai_codex_auth_status(status: RuntimeOpenAiCodexAuthStatus) -> ProviderAuthStatus {
    let mut mapped = ProviderAuthStatus {
        state: ProviderAuthState::SignedOut,
        plan_type: status.plan_type,
        last_refresh: status.last_refresh_unix.map(|value| value.to_string()),
        auth_url: None,
        verification_url: None,
        user_code: None,
        error: status.error,
    };

    mapped.state = match status.state {
        RuntimeOpenAiCodexLoginState::SignedOut => ProviderAuthState::SignedOut,
        RuntimeOpenAiCodexLoginState::BrowserPending(pending) => {
            mapped.auth_url = Some(pending.auth_url);
            ProviderAuthState::BrowserPending
        }
        RuntimeOpenAiCodexLoginState::DeviceCodePending(pending) => {
            mapped.verification_url = Some(pending.verification_url);
            mapped.user_code = Some(pending.user_code);
            ProviderAuthState::DeviceCodePending
        }
        RuntimeOpenAiCodexLoginState::Authenticated => ProviderAuthState::Authenticated,
    };

    mapped
}
