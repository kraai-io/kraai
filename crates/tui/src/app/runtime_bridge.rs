use agent_runtime::RuntimeHandle;
use crossbeam_channel::{Receiver, Sender, unbounded};

use super::{RuntimeRequest, RuntimeResponse};

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
                RuntimeRequest::GetSettings => {
                    let result = rt
                        .block_on(runtime.get_settings())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::Settings(result));
                }
                RuntimeRequest::SendMessage {
                    message,
                    model_id,
                    provider_id,
                } => {
                    let result = rt
                        .block_on(runtime.send_message(message, model_id, provider_id))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::SendMessage(result));
                }
                RuntimeRequest::SaveSettings { settings } => {
                    let result = rt
                        .block_on(runtime.save_settings(settings))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::SaveSettings(result));
                }
                RuntimeRequest::GetChatHistory => {
                    let result = rt
                        .block_on(runtime.get_chat_history())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::ChatHistory(result));
                }
                RuntimeRequest::GetCurrentTip => {
                    let result = rt
                        .block_on(runtime.get_current_tip())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::CurrentTip(result));
                }
                RuntimeRequest::ClearCurrentSession => {
                    let result = rt
                        .block_on(runtime.clear_current_session())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::ClearCurrentSession(result));
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
                RuntimeRequest::GetCurrentSessionId => {
                    let result = rt
                        .block_on(runtime.get_current_session_id())
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::CurrentSessionId(result));
                }
                RuntimeRequest::ApproveTool { call_id } => {
                    let result = rt
                        .block_on(runtime.approve_tool(call_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::ApproveTool { call_id, result });
                }
                RuntimeRequest::DenyTool { call_id } => {
                    let result = rt
                        .block_on(runtime.deny_tool(call_id.clone()))
                        .map_err(|e| e.to_string());
                    let _ = res_tx.send(RuntimeResponse::DenyTool { call_id, result });
                }
                RuntimeRequest::ExecuteApprovedTools => {
                    let result = rt
                        .block_on(runtime.execute_approved_tools())
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
        RuntimeRequest::GetSettings => RuntimeResponse::Settings(Err(message.to_string())),
        RuntimeRequest::SendMessage { .. } => {
            RuntimeResponse::SendMessage(Err(message.to_string()))
        }
        RuntimeRequest::SaveSettings { .. } => {
            RuntimeResponse::SaveSettings(Err(message.to_string()))
        }
        RuntimeRequest::GetChatHistory => RuntimeResponse::ChatHistory(Err(message.to_string())),
        RuntimeRequest::GetCurrentTip => RuntimeResponse::CurrentTip(Err(message.to_string())),
        RuntimeRequest::ClearCurrentSession => {
            RuntimeResponse::ClearCurrentSession(Err(message.to_string()))
        }
        RuntimeRequest::LoadSession { session_id } => RuntimeResponse::LoadSession {
            session_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::ListSessions => RuntimeResponse::Sessions(Err(message.to_string())),
        RuntimeRequest::DeleteSession { session_id } => RuntimeResponse::DeleteSession {
            session_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::GetCurrentSessionId => {
            RuntimeResponse::CurrentSessionId(Err(message.to_string()))
        }
        RuntimeRequest::ApproveTool { call_id } => RuntimeResponse::ApproveTool {
            call_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::DenyTool { call_id } => RuntimeResponse::DenyTool {
            call_id,
            result: Err(message.to_string()),
        },
        RuntimeRequest::ExecuteApprovedTools => {
            RuntimeResponse::ExecuteApprovedTools(Err(message.to_string()))
        }
    };

    let _ = res_tx.send(response);
}
