use agent_runtime::{
    OpenAiCodexAuthStatus as RuntimeOpenAiCodexAuthStatus,
    OpenAiCodexLoginState as RuntimeOpenAiCodexLoginState, RuntimeHandle,
};
use crossbeam_channel::{unbounded, Receiver, Sender};

use super::{ProviderAuthState, ProviderAuthStatus, RuntimeRequest, RuntimeResponse};

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
                    let result = rt
                        .block_on(runtime.list_models())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::Models(result));
                }
                RuntimeRequest::ListProviderDefinitions => {
                    let result = rt
                        .block_on(runtime.list_provider_definitions())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::ProviderDefinitions(result));
                }
                RuntimeRequest::GetSettings => {
                    let result = rt
                        .block_on(runtime.get_settings())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::Settings(result));
                }
                RuntimeRequest::GetOpenAiCodexAuthStatus => {
                    let result = rt
                        .block_on(runtime.get_openai_codex_auth_status())
                        .map(map_openai_codex_auth_status)
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::OpenAiCodexAuthStatus(result));
                }
                RuntimeRequest::StartOpenAiCodexBrowserLogin => {
                    let result = rt
                        .block_on(runtime.start_openai_codex_browser_login())
                        .map_err(|e| e.to_string())
                        .and_then(|_| {
                            rt.block_on(runtime.get_openai_codex_auth_status())
                                .map(map_openai_codex_auth_status)
                                .map_err(|e| e.to_string())
                        });
                    let _ = res_tx.send(RuntimeResponse::StartOpenAiCodexBrowserLogin(result));
                }
                RuntimeRequest::StartOpenAiCodexDeviceCodeLogin => {
                    let result = rt
                        .block_on(runtime.start_openai_codex_device_code_login())
                        .map_err(|e| e.to_string())
                        .and_then(|_| {
                            rt.block_on(runtime.get_openai_codex_auth_status())
                                .map(map_openai_codex_auth_status)
                                .map_err(|e| e.to_string())
                        });
                    let _ = res_tx.send(RuntimeResponse::StartOpenAiCodexDeviceCodeLogin(result));
                }
                RuntimeRequest::CancelOpenAiCodexLogin => {
                    let result = rt
                        .block_on(runtime.cancel_openai_codex_login())
                        .map_err(|e| e.to_string())
                        .and_then(|_| {
                            rt.block_on(runtime.get_openai_codex_auth_status())
                                .map(map_openai_codex_auth_status)
                                .map_err(|e| e.to_string())
                        });
                    let _ = res_tx.send(RuntimeResponse::CancelOpenAiCodexLogin(result));
                }
                RuntimeRequest::LogoutOpenAiCodexAuth => {
                    let result = rt
                        .block_on(runtime.logout_openai_codex_auth())
                        .map_err(|e| e.to_string())
                        .and_then(|_| {
                            rt.block_on(runtime.get_openai_codex_auth_status())
                                .map(map_openai_codex_auth_status)
                                .map_err(|e| e.to_string())
                        });
                    let _ = res_tx.send(RuntimeResponse::LogoutOpenAiCodexAuth(result));
                }
                RuntimeRequest::ListAgentProfiles { session_id } => {
                    let result = rt
                        .block_on(runtime.list_agent_profiles(session_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::AgentProfiles { session_id, result });
                }
                RuntimeRequest::SetSessionProfile {
                    session_id,
                    profile_id,
                } => {
                    let result = rt
                        .block_on(runtime.set_session_profile(session_id, profile_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::SetSessionProfile { profile_id, result });
                }
                RuntimeRequest::CreateSession => {
                    let result = rt
                        .block_on(runtime.create_session())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::CreateSession(result));
                }
                RuntimeRequest::SendMessage {
                    session_id,
                    message,
                    model_id,
                    provider_id,
                    auto_approve,
                } => {
                    let result = rt
                        .block_on(runtime.send_message_with_options(
                            session_id,
                            message,
                            model_id,
                            provider_id,
                            auto_approve,
                        ))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::SendMessage(result));
                }
                RuntimeRequest::SaveSettings { settings } => {
                    let result = rt
                        .block_on(runtime.save_settings(settings))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::SaveSettings(result));
                }
                RuntimeRequest::GetChatHistory { session_id } => {
                    let result = rt
                        .block_on(runtime.get_chat_history(session_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::ChatHistory { session_id, result });
                }
                RuntimeRequest::GetCurrentTip { session_id } => {
                    let result = rt
                        .block_on(runtime.get_tip(session_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::CurrentTip { session_id, result });
                }
                RuntimeRequest::GetPendingTools { session_id } => {
                    let result = rt
                        .block_on(runtime.get_pending_tools(session_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::PendingTools { session_id, result });
                }
                RuntimeRequest::LoadSession { session_id } => {
                    let result = rt
                        .block_on(runtime.load_session(session_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::LoadSession { session_id, result });
                }
                RuntimeRequest::ListSessions => {
                    let result = rt
                        .block_on(runtime.list_sessions())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::Sessions(result));
                }
                RuntimeRequest::DeleteSession { session_id } => {
                    let result = rt
                        .block_on(runtime.delete_session(session_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::DeleteSession { session_id, result });
                }
                RuntimeRequest::ApproveTool {
                    session_id,
                    call_id,
                } => {
                    let result = rt
                        .block_on(runtime.approve_tool(session_id, call_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::ApproveTool { call_id, result });
                }
                RuntimeRequest::DenyTool {
                    session_id,
                    call_id,
                } => {
                    let result = rt
                        .block_on(runtime.deny_tool(session_id, call_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::DenyTool { call_id, result });
                }
                RuntimeRequest::CancelStream { session_id } => {
                    let result = rt
                        .block_on(runtime.cancel_stream(session_id))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::CancelStream(result));
                }
                RuntimeRequest::ExecuteApprovedTools { session_id } => {
                    let result = rt
                        .block_on(runtime.execute_approved_tools(session_id))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::ExecuteApprovedTools(result));
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
    let response = match req {
        RuntimeRequest::ListModels => RuntimeResponse::Models(Err(message.to_string())),
        RuntimeRequest::ListProviderDefinitions => {
            RuntimeResponse::ProviderDefinitions(Err(message.to_string()))
        }
        RuntimeRequest::GetSettings => RuntimeResponse::Settings(Err(message.to_string())),
        RuntimeRequest::GetOpenAiCodexAuthStatus => {
            RuntimeResponse::OpenAiCodexAuthStatus(Err(message.to_string()))
        }
        RuntimeRequest::StartOpenAiCodexBrowserLogin => {
            RuntimeResponse::StartOpenAiCodexBrowserLogin(Err(message.to_string()))
        }
        RuntimeRequest::StartOpenAiCodexDeviceCodeLogin => {
            RuntimeResponse::StartOpenAiCodexDeviceCodeLogin(Err(message.to_string()))
        }
        RuntimeRequest::CancelOpenAiCodexLogin => {
            RuntimeResponse::CancelOpenAiCodexLogin(Err(message.to_string()))
        }
        RuntimeRequest::LogoutOpenAiCodexAuth => {
            RuntimeResponse::LogoutOpenAiCodexAuth(Err(message.to_string()))
        }
        RuntimeRequest::ListAgentProfiles { session_id } => RuntimeResponse::AgentProfiles {
            session_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::SetSessionProfile { profile_id, .. } => {
            RuntimeResponse::SetSessionProfile {
                profile_id,
                result: Err(message.to_string()),
            }
        }
        RuntimeRequest::CreateSession => RuntimeResponse::CreateSession(Err(message.to_string())),
        RuntimeRequest::SendMessage { .. } => {
            RuntimeResponse::SendMessage(Err(message.to_string()))
        }
        RuntimeRequest::SaveSettings { .. } => {
            RuntimeResponse::SaveSettings(Err(message.to_string()))
        }
        RuntimeRequest::GetChatHistory { session_id } => RuntimeResponse::ChatHistory {
            session_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::GetCurrentTip { session_id } => RuntimeResponse::CurrentTip {
            session_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::GetPendingTools { session_id } => RuntimeResponse::PendingTools {
            session_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::LoadSession { session_id } => RuntimeResponse::LoadSession {
            session_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::ListSessions => RuntimeResponse::Sessions(Err(message.to_string())),
        RuntimeRequest::DeleteSession { session_id } => RuntimeResponse::DeleteSession {
            session_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::ApproveTool { call_id, .. } => RuntimeResponse::ApproveTool {
            call_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::DenyTool { call_id, .. } => RuntimeResponse::DenyTool {
            call_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::CancelStream { .. } => {
            RuntimeResponse::CancelStream(Err(message.to_string()))
        }
        RuntimeRequest::ExecuteApprovedTools { .. } => {
            RuntimeResponse::ExecuteApprovedTools(Err(message.to_string()))
        }
    };

    let _ = res_tx.send(response);
}

fn map_openai_codex_auth_status(status: RuntimeOpenAiCodexAuthStatus) -> ProviderAuthStatus {
    let mut mapped = ProviderAuthStatus {
        state: ProviderAuthState::SignedOut,
        email: status.email,
        plan_type: status.plan_type,
        account_id: status.account_id,
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
