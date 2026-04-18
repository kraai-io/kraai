use std::collections::HashMap;

use color_eyre::eyre::{Result, eyre};

use super::config::{canonicalize_workspace_dir, map_openai_codex_auth_status};
use super::core::RuntimeCore;
use crate::api::{Event, Model, PendingToolInfo, Session, SessionContextUsage, WorkspaceState};
use crate::handle::Command;
use crate::settings::read_settings_document;

impl RuntimeCore {
    pub(crate) async fn handle_command(&self, command: Command) -> Result<()> {
        match command {
            Command::ListModels { response } => {
                let models_map = self.agent_manager.lock().await.list_models().await;
                let models: HashMap<String, Vec<Model>> = models_map
                    .into_iter()
                    .map(|(provider_id, model_list)| {
                        let models = model_list
                            .into_iter()
                            .map(|model| Model {
                                id: model.id.to_string(),
                                name: model.name,
                                max_context: model.max_context,
                            })
                            .collect();
                        (provider_id.to_string(), models)
                    })
                    .collect();
                response
                    .send(models)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::ListProviderDefinitions { response } => {
                response
                    .send(self.provider_registry.list_definitions())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::GetSettings { response } => {
                let settings =
                    read_settings_document(&self.provider_config_path, &self.provider_registry)?;
                response
                    .send(settings)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::ListAgentProfiles {
                session_id,
                response,
            } => {
                let profiles = self
                    .agent_manager
                    .lock()
                    .await
                    .list_agent_profiles(&session_id)
                    .await?;
                response
                    .send(profiles)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::SetSessionProfile {
                session_id,
                profile_id,
                response,
            } => {
                self.agent_manager
                    .lock()
                    .await
                    .set_session_profile(&session_id, profile_id)
                    .await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::SaveSettings { settings, response } => {
                self.save_settings_document(settings).await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::CreateSession { response } => {
                let session_id = self.agent_manager.lock().await.create_session().await?;
                response
                    .send(session_id)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::LoadConfig => {
                self.load_providers_config().await?;
                tracing::info!("Loaded config");
                self.send_event(Event::ConfigLoaded);
            }
            Command::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
                auto_approve,
            } => {
                self.handle_send_message(session_id, message, model_id, provider_id, auto_approve)
                    .await;
            }
            Command::StartQueuedMessages { session_id } => {
                self.handle_start_queued_messages(session_id).await;
            }
            Command::LoadSession {
                session_id,
                response,
            } => {
                let loaded = self
                    .agent_manager
                    .lock()
                    .await
                    .prepare_session(&session_id)
                    .await?;
                response
                    .send(loaded)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::ListSessions { response } => {
                let agent = self.agent_manager.lock().await;
                let sessions = agent.list_sessions().await?;
                let streaming_sessions = agent.streaming_session_ids().await;
                let sessions = sessions
                    .into_iter()
                    .map(|session| Session {
                        profile_locked: agent.is_profile_locked(&session.id),
                        waiting_for_approval: agent.session_waiting_for_approval(&session.id),
                        is_streaming: streaming_sessions.contains(&session.id),
                        ..Session::from_session_meta(session)
                    })
                    .collect();
                response
                    .send(sessions)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::DeleteSession { session_id } => {
                if let Some(active_stream) = self.take_active_stream(&session_id).await {
                    active_stream.abort_handle.abort();
                }
                self.queued_messages.lock().await.remove(&session_id);
                self.agent_manager
                    .lock()
                    .await
                    .delete_session(&session_id)
                    .await?;
            }
            Command::GetWorkspaceState {
                session_id,
                response,
            } => {
                let workspace_state = self
                    .agent_manager
                    .lock()
                    .await
                    .get_workspace_dir_state(&session_id)
                    .await?
                    .map(|(workspace_dir, applies_next_chat)| WorkspaceState {
                        workspace_dir: workspace_dir.display().to_string(),
                        applies_next_chat,
                    });
                response
                    .send(workspace_state)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::SetWorkspaceDir {
                session_id,
                workspace_dir,
                response,
            } => {
                let workspace_dir = canonicalize_workspace_dir(&workspace_dir)?;
                self.agent_manager
                    .lock()
                    .await
                    .set_workspace_dir(&session_id, workspace_dir)
                    .await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::GetTip {
                session_id,
                response,
            } => {
                let tip_id = self
                    .agent_manager
                    .lock()
                    .await
                    .get_tip(&session_id)
                    .await?
                    .map(|id| id.to_string());
                response
                    .send(tip_id)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::UndoLastUserMessage {
                session_id,
                response,
            } => {
                let restored_message = self
                    .agent_manager
                    .lock()
                    .await
                    .undo_last_user_message(&session_id)
                    .await?;
                response
                    .send(restored_message)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::GetChatHistory {
                session_id,
                response,
            } => {
                let history = self
                    .agent_manager
                    .lock()
                    .await
                    .get_chat_history(&session_id)
                    .await?;
                response
                    .send(history)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::GetSessionContextUsage {
                session_id,
                response,
            } => {
                let usage = self
                    .agent_manager
                    .lock()
                    .await
                    .get_session_context_usage(&session_id)
                    .await?
                    .map(|usage| SessionContextUsage {
                        provider_id: usage.provider_id.to_string(),
                        model_id: usage.model_id.to_string(),
                        max_context: usage.max_context,
                        usage: usage.usage,
                    });
                response
                    .send(usage)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::GetPendingTools {
                session_id,
                response,
            } => {
                let tools = self
                    .agent_manager
                    .lock()
                    .await
                    .list_pending_tools(&session_id)
                    .into_iter()
                    .map(|tool| PendingToolInfo {
                        call_id: tool.call_id.to_string(),
                        tool_id: tool.tool_id.to_string(),
                        args: serde_json::to_string(&tool.args).unwrap_or_default(),
                        description: tool.description,
                        risk_level: tool.risk_level.as_str().to_string(),
                        reasons: tool.reasons,
                        approved: tool.approved,
                        queue_order: tool.queue_order,
                    })
                    .collect();
                response
                    .send(tools)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::ApproveTool {
                session_id,
                call_id,
            } => {
                let call_id = kraai_types::CallId::new(call_id);
                self.agent_manager
                    .lock()
                    .await
                    .approve_tool(&session_id, call_id);
            }
            Command::DenyTool {
                session_id,
                call_id,
            } => {
                let call_id = kraai_types::CallId::new(call_id);
                self.agent_manager
                    .lock()
                    .await
                    .deny_tool(&session_id, call_id);
            }
            Command::CancelStream {
                session_id,
                response,
            } => {
                let cancelled = self.cancel_stream(session_id).await?;
                response
                    .send(cancelled)
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::ContinueSession { session_id } => {
                self.start_continuation(session_id).await;
            }
            Command::ExecuteApprovedTools { session_id } => {
                self.handle_execute_tools(session_id).await;
            }
            Command::GetOpenAiCodexAuthStatus { response } => {
                response
                    .send(map_openai_codex_auth_status(
                        self.openai_codex_auth.get_status().await,
                    ))
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::StartOpenAiCodexBrowserLogin { response } => {
                self.openai_codex_auth.start_browser_login().await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::StartOpenAiCodexDeviceCodeLogin { response } => {
                self.openai_codex_auth.start_device_code_login().await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::CancelOpenAiCodexLogin { response } => {
                self.openai_codex_auth.cancel_login().await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
            Command::LogoutOpenAiCodexAuth { response } => {
                self.openai_codex_auth.logout().await?;
                response
                    .send(())
                    .map_err(|_| eyre!("Failed to send response"))?;
            }
        }

        Ok(())
    }
}
