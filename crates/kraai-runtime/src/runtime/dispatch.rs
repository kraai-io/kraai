use std::collections::HashMap;

use color_eyre::eyre::{Result, eyre};
use tokio::sync::oneshot;

use super::config::{canonicalize_workspace_dir, map_openai_codex_auth_status};
use super::core::RuntimeCore;
use crate::api::{
    AgentProfileCatalog, Model, RuntimeError, RuntimeResult, Session, SessionActivity,
    SessionContextUsage, SessionSnapshot, WorkspaceState,
};
use crate::handle::Command;
use crate::settings::read_settings_document;

fn respond<T>(response: oneshot::Sender<RuntimeResult<T>>, result: Result<T>) {
    let _ = response.send(result.map_err(RuntimeError::from_report));
}

impl RuntimeCore {
    async fn build_session_snapshot(&self, session_id: &str) -> Result<SessionSnapshot> {
        let _snapshot_guard = self.session_state_barrier.write().await;
        let pending_script = self.get_pending_script(session_id).await;
        let executing_script = self.has_active_script_tasks(session_id).await;
        let queued_messages = self
            .queued_messages
            .lock()
            .await
            .get(session_id)
            .map_or(0, std::collections::VecDeque::len);
        let active_stream = self.active_streams.lock().await.contains_key(session_id);

        let mut agent = self.agent_manager.write().await;
        let session_meta = agent
            .list_sessions()
            .await?
            .into_iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| {
                eyre!(kraai_types::DomainError::not_found(format!(
                    "Session not found: {session_id}"
                )))
            })?;
        let history = agent.get_chat_history(session_id).await?;
        let context_usage = agent
            .get_session_context_usage(session_id)
            .await?
            .map(|usage| SessionContextUsage {
                provider_id: usage.provider_id.to_string(),
                model_id: usage.model_id.to_string(),
                max_context: usage.max_context,
                usage: usage.usage,
            });
        let profiles = agent.list_agent_profiles(session_id).await?;
        let profile_locked = agent.is_profile_locked(session_id);
        let streaming = active_stream || agent.streaming_session_ids().await.contains(session_id);
        drop(agent);

        let activity = if pending_script.is_some() {
            SessionActivity::AwaitingApproval
        } else if executing_script {
            SessionActivity::ExecutingScript
        } else if streaming {
            SessionActivity::Streaming
        } else {
            SessionActivity::Idle
        };
        let session = Session {
            profile_locked,
            waiting_for_approval: pending_script.is_some(),
            is_streaming: streaming,
            ..Session::from_session_meta(session_meta)
        };
        // State mutations hold a shared barrier guard until their events are published. Reading
        // the sequence last therefore gives clients a stable incremental recovery boundary.
        let event_sequence = self.event_tx.latest_sequence();

        Ok(SessionSnapshot {
            event_sequence,
            session,
            history,
            context_usage,
            pending_script,
            profiles,
            activity,
            queued_messages,
        })
    }

    pub(crate) async fn handle_command(&self, command: Command) -> Result<()> {
        let _state_guard = if matches!(&command, Command::GetSessionSnapshot { .. }) {
            None
        } else {
            Some(self.session_state_barrier.read().await)
        };
        match command {
            Command::ListModels { response } => {
                let models_map = self.agent_manager.read().await.list_models().await;
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
                let _ = response.send(Ok(models));
            }
            Command::ListProviderDefinitions { response } => {
                let _ = response.send(Ok(self.provider_registry.list_definitions()));
            }
            Command::GetSettings { response } => {
                respond(
                    response,
                    read_settings_document(&self.provider_config_path, &self.provider_registry),
                );
            }
            Command::ListAgentProfiles {
                session_id,
                response,
            } => {
                let profiles = self
                    .agent_manager
                    .write()
                    .await
                    .list_agent_profiles(&session_id)
                    .await;
                respond(response, profiles);
            }
            Command::GetAgentProfileCatalog {
                workspace_dir,
                response,
            } => {
                let result = async {
                    let workspace_dir = workspace_dir
                        .as_deref()
                        .map(canonicalize_workspace_dir)
                        .transpose()?;
                    let agent = self.agent_manager.read().await;
                    let (workspace_dir, profiles) =
                        agent.list_agent_profiles_for_workspace(workspace_dir.as_deref());
                    drop(agent);
                    let default_profile_id = profiles
                        .selected_profile_id
                        .clone()
                        .ok_or_else(|| eyre!("Agent profile catalog has no default profile"))?;
                    Ok(AgentProfileCatalog {
                        workspace_dir: workspace_dir.display().to_string(),
                        default_profile_id,
                        profiles: profiles.profiles,
                        warnings: profiles.warnings,
                    })
                }
                .await;
                respond(response, result);
            }
            Command::SetSessionProfile {
                session_id,
                profile_id,
                response,
            } => {
                let result = self
                    .agent_manager
                    .write()
                    .await
                    .set_session_profile(&session_id, profile_id)
                    .await;
                respond(response, result);
            }
            Command::SaveSettings { settings, response } => {
                let result = self.save_settings_document(settings).await;
                let _ = response.send(result);
            }
            Command::CreateSession { request, response } => {
                let result = async {
                    let workspace_dir = request
                        .workspace_dir
                        .as_deref()
                        .map(canonicalize_workspace_dir)
                        .transpose()?;
                    self.agent_manager
                        .write()
                        .await
                        .create_session_with(workspace_dir, request.profile_id)
                        .await
                }
                .await;
                respond(response, result);
            }
            Command::LoadConfig => {
                self.load_providers_config_and_emit().await?;
            }
            Command::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
                response,
            } => {
                let result = self
                    .handle_send_message(session_id, message, model_id, provider_id)
                    .await;
                let _ = response.send(result);
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
                    .write()
                    .await
                    .prepare_session(&session_id)
                    .await;
                respond(response, loaded);
            }
            Command::ListSessions { response } => {
                let result = async {
                    let pending_approvals = self
                        .pending_script_approvals
                        .lock()
                        .await
                        .keys()
                        .cloned()
                        .collect::<std::collections::HashSet<_>>();
                    let agent = self.agent_manager.read().await;
                    let sessions = agent.list_sessions().await?;
                    let streaming_sessions = agent.streaming_session_ids().await;
                    let profile_locked_sessions = sessions
                        .iter()
                        .filter(|session| agent.is_profile_locked(&session.id))
                        .map(|session| session.id.clone())
                        .collect::<std::collections::HashSet<_>>();
                    drop(agent);
                    let sessions = sessions
                        .into_iter()
                        .map(|session| Session {
                            profile_locked: profile_locked_sessions.contains(&session.id),
                            waiting_for_approval: pending_approvals.contains(&session.id),
                            is_streaming: streaming_sessions.contains(&session.id),
                            ..Session::from_session_meta(session)
                        })
                        .collect();
                    Ok(sessions)
                }
                .await;
                respond(response, result);
            }
            Command::ListUserInputHistory { limit, response } => {
                let history = self
                    .agent_manager
                    .read()
                    .await
                    .list_user_input_history(limit)
                    .await;
                respond(response, history);
            }
            Command::DeleteSession {
                session_id,
                response,
            } => {
                if self.has_active_script_tasks(&session_id).await
                    || self
                        .pending_script_approvals
                        .lock()
                        .await
                        .contains_key(&session_id)
                {
                    let _ = response.send(Err(RuntimeError::conflict(format!(
                        "Cannot delete session {session_id} while a script is active"
                    ))));
                    return Ok(());
                }
                if let Some(active_stream) = self.take_active_stream(&session_id).await {
                    active_stream.abort_handle.abort();
                }
                self.queued_messages.lock().await.remove(&session_id);
                let result = self
                    .agent_manager
                    .write()
                    .await
                    .delete_session(&session_id)
                    .await;
                respond(response, result);
            }
            Command::GetWorkspaceState {
                session_id,
                response,
            } => {
                let result = self
                    .agent_manager
                    .write()
                    .await
                    .get_workspace_dir_state(&session_id)
                    .await
                    .map(|workspace_state| {
                        workspace_state.map(|(workspace_dir, applies_next_chat)| WorkspaceState {
                            workspace_dir: workspace_dir.display().to_string(),
                            applies_next_chat,
                        })
                    });
                respond(response, result);
            }
            Command::SetWorkspaceDir {
                session_id,
                workspace_dir,
                response,
            } => {
                let result = async {
                    let workspace_dir = canonicalize_workspace_dir(&workspace_dir)?;
                    self.agent_manager
                        .write()
                        .await
                        .set_workspace_dir(&session_id, workspace_dir)
                        .await
                }
                .await;
                respond(response, result);
            }
            Command::GetTip {
                session_id,
                response,
            } => {
                let tip_id = self
                    .agent_manager
                    .read()
                    .await
                    .get_tip(&session_id)
                    .await
                    .map(|tip_id| tip_id.map(|id| id.to_string()));
                respond(response, tip_id);
            }
            Command::UndoLastUserMessage {
                session_id,
                response,
            } => {
                let restored_message = self
                    .agent_manager
                    .read()
                    .await
                    .undo_last_user_message(&session_id)
                    .await;
                respond(response, restored_message);
            }
            Command::GetChatHistory {
                session_id,
                response,
            } => {
                let history = self
                    .agent_manager
                    .read()
                    .await
                    .get_chat_history(&session_id)
                    .await;
                respond(response, history);
            }
            Command::GetSessionSnapshot {
                session_id,
                response,
            } => {
                let runtime = self.clone();
                tokio::spawn(async move {
                    let result = runtime.build_session_snapshot(&session_id).await;
                    respond(response, result);
                });
            }
            Command::GetSessionContextUsage {
                session_id,
                response,
            } => {
                let usage = self
                    .agent_manager
                    .read()
                    .await
                    .get_session_context_usage(&session_id)
                    .await
                    .map(|usage| {
                        usage.map(|usage| SessionContextUsage {
                            provider_id: usage.provider_id.to_string(),
                            model_id: usage.model_id.to_string(),
                            max_context: usage.max_context,
                            usage: usage.usage,
                        })
                    });
                respond(response, usage);
            }
            Command::GetPendingScript {
                session_id,
                response,
            } => {
                let result = self.get_pending_script(&session_id).await;
                let _ = response.send(Ok(result));
            }
            Command::ApproveScript {
                session_id,
                execution_id,
                response,
            } => {
                let result = self.approve_pending_script(session_id, execution_id).await;
                respond(response, result);
            }
            Command::DenyScript {
                session_id,
                execution_id,
                response,
            } => {
                let result = self.deny_pending_script(session_id, execution_id).await;
                respond(response, result);
            }
            Command::CancelStream {
                session_id,
                response,
            } => {
                let cancelled = self.cancel_stream(session_id).await;
                respond(response, cancelled);
            }
            Command::ContinueSession {
                session_id,
                response,
            } => {
                let result = self.start_continuation(session_id).await;
                let _ = response.send(result);
            }
            Command::GetOpenAiCodexAuthStatus { response } => {
                let _ = response.send(Ok(map_openai_codex_auth_status(
                    self.openai_codex_auth.get_status().await,
                )));
            }
            Command::StartOpenAiCodexBrowserLogin { response } => {
                let result = self.openai_codex_auth.start_browser_login().await;
                respond(response, result.map(|_| ()).map_err(Into::into));
            }
            Command::StartOpenAiCodexDeviceCodeLogin { response } => {
                let result = self.openai_codex_auth.start_device_code_login().await;
                respond(response, result.map(|_| ()).map_err(Into::into));
            }
            Command::CancelOpenAiCodexLogin { response } => {
                let result = self.openai_codex_auth.cancel_login().await;
                respond(response, result.map(|_| ()).map_err(Into::into));
            }
            Command::LogoutOpenAiCodexAuth { response } => {
                let result = self.openai_codex_auth.logout().await;
                respond(response, result.map(|_| ()).map_err(Into::into));
            }
            Command::Shutdown { .. } => {
                return Err(eyre!("Shutdown command reached normal dispatch"));
            }
        }

        Ok(())
    }
}
