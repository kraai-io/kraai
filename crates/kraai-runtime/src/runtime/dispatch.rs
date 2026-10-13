use std::collections::HashMap;

use color_eyre::eyre::{Result, eyre};
use tokio::sync::oneshot;

use super::config::canonicalize_workspace_dir;
use super::core::RuntimeCore;
use super::session::{ActivityKind, SessionMessage};
use crate::api::{
    AgentProfileCatalog, Model, RuntimeError, RuntimeResult, Session, SessionContextUsage,
    WorkspaceState,
};
use crate::handle::Command;
use crate::settings::read_settings_document;
use kraai_types::ModelSelection;

fn respond<T>(response: oneshot::Sender<RuntimeResult<T>>, result: Result<T>) {
    let _ = response.send(result.map_err(RuntimeError::from_report));
}

impl RuntimeCore {
    /// Reads a page of history ending at `from`, or at the session's latest message.
    async fn history_page(
        &self,
        session_id: &kraai_types::SessionId,
        from: Option<kraai_types::MessageId>,
        limit: usize,
    ) -> Result<crate::HistoryPage> {
        let from = match from {
            Some(from) => Some(from),
            None => self.agent_manager.get_tip(session_id).await?,
        };
        let Some(from) = from else {
            return Ok(crate::HistoryPage {
                messages: std::collections::BTreeMap::new(),
                older_history: None,
            });
        };
        let (messages, older_history) = self
            .agent_manager
            .history_page(&from, limit.min(super::session::SNAPSHOT_HISTORY_LIMIT))
            .await?;
        Ok(crate::HistoryPage {
            messages: messages
                .into_iter()
                .map(|message| {
                    (
                        message.id.clone(),
                        super::client_view::client_message(message),
                    )
                })
                .collect(),
            older_history,
        })
    }

    /// Routes session commands to their session's actor in arrival order and runs every
    /// other command concurrently.
    pub(crate) fn dispatch(&self, command: Command) {
        let (session_id, message) = match command {
            Command::SendMessage {
                session_id,
                message,
                model_id,
                provider_id,
                options,
                response,
            } => (
                session_id,
                SessionMessage::Send {
                    message,
                    selection: ModelSelection {
                        provider_id,
                        model_id,
                        options,
                    },
                    response,
                },
            ),
            Command::ContinueSession {
                session_id,
                model_id,
                provider_id,
                options,
                wait_for_preparation,
                response,
            } => (
                session_id,
                SessionMessage::Continue {
                    selection: Some(ModelSelection {
                        provider_id,
                        model_id,
                        options,
                    }),
                    wait_for_preparation,
                    response,
                },
            ),
            Command::CancelStream {
                session_id,
                response,
            } => (
                session_id,
                SessionMessage::Cancel {
                    discard_queue: false,
                    expected_lease: None,
                    response: Some(response),
                },
            ),
            Command::CancelTurn {
                session_id,
                response,
            } => (
                session_id,
                SessionMessage::Cancel {
                    discard_queue: true,
                    expected_lease: None,
                    response: Some(response),
                },
            ),
            Command::ApproveScript {
                session_id,
                execution_id,
                response,
            } => (
                session_id,
                SessionMessage::Approve {
                    execution_id,
                    response,
                },
            ),
            Command::DenyScript {
                session_id,
                execution_id,
                response,
            } => (
                session_id,
                SessionMessage::Deny {
                    execution_id,
                    response,
                },
            ),
            Command::GetPendingScript {
                session_id,
                response,
            } => (session_id, SessionMessage::PendingScript { response }),
            Command::GetSessionSnapshot {
                session_id,
                response,
            } => (session_id, SessionMessage::Snapshot { response }),
            Command::SetSessionMcpServers {
                session_id,
                config,
                response,
            } => (
                session_id,
                SessionMessage::SetMcpServers { config, response },
            ),
            Command::UndoLastUserMessage {
                session_id,
                response,
            } => (session_id, SessionMessage::Undo { response }),
            Command::DeleteSession {
                session_id,
                response,
            } => (session_id, SessionMessage::Delete { response }),
            command => {
                let runtime = self.clone();
                tokio::spawn(async move {
                    if let Err(error) = runtime.handle_command(command).await {
                        runtime.send_service_error(error);
                    }
                });
                return;
            }
        };
        if self.is_stopping() {
            message.reject();
        } else {
            self.sessions.send(self, &session_id, message);
        }
    }

    pub(crate) async fn handle_command(&self, command: Command) -> Result<()> {
        match command {
            Command::ListModels { response } => {
                let models_map = self.agent_manager.list_models().await;
                let models: HashMap<String, Vec<Model>> = models_map
                    .into_iter()
                    .map(|(provider_id, model_list)| {
                        let models = model_list
                            .into_iter()
                            .map(|model| Model {
                                id: model.id.to_string(),
                                name: model.name,
                                max_context: model.max_context,
                                options: model.options,
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
                    read_settings_document(
                        &self.config.provider_config_path,
                        &self.provider_registry,
                    )
                    .await,
                );
            }
            Command::ListAgentProfiles {
                session_id,
                response,
            } => {
                let profiles = self.agent_manager.list_agent_profiles(&session_id).await;
                respond(response, profiles);
            }
            Command::GetAgentProfileCatalog {
                workspace_dir,
                response,
            } => {
                let agent_manager = self.agent_manager.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let workspace_dir = workspace_dir
                        .as_deref()
                        .map(canonicalize_workspace_dir)
                        .transpose()?;
                    let agent = &agent_manager;
                    let (workspace_dir, profiles) =
                        agent.list_agent_profiles_for_workspace(workspace_dir.as_deref());
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
                })
                .await
                .unwrap_or_else(|error| Err(error.into()));
                respond(response, result);
            }
            Command::SetSessionProfile {
                session_id,
                profile_id,
                response,
            } => {
                let result = self
                    .agent_manager
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
                        .create_session_with(workspace_dir, request.profile_id)
                        .await
                }
                .await;
                respond(response, result);
            }
            Command::LoadConfig => {
                self.load_providers_config_and_emit().await?;
            }
            Command::ImportImage { bytes, response } => {
                let result = self
                    .image_store
                    .import(bytes)
                    .await
                    .map_err(crate::RuntimeError::internal);
                let _ = response.send(result);
            }
            Command::ReadImage { image, response } => {
                respond(response, self.image_store.read(&image).await);
            }
            Command::LoadSession {
                session_id,
                response,
            } => {
                if let Some(observation) = self.session_store.observe(&session_id).await? {
                    let mut watched = self.observed_sessions.lock().await;
                    watched.clear();
                    watched.insert(session_id.clone(), observation);
                }
                let loaded = self.agent_manager.prepare_session(&session_id).await;
                respond(response, loaded);
            }
            Command::ListSessions { response } => {
                let result = async {
                    let agent = &self.agent_manager;
                    let sessions = agent.list_sessions().await?;
                    let streaming_sessions = agent.streaming_session_ids().await;
                    let mut sessions: Vec<_> = sessions
                        .into_iter()
                        .map(|session| Session {
                            profile_locked: agent.is_profile_locked(&session.id),
                            waiting_for_approval: self.sessions.activity(&session.id)
                                == ActivityKind::AwaitingApproval,
                            is_streaming: streaming_sessions.contains(&session.id),
                            is_running: agent.is_turn_active(&session.id),
                            ..Session::from_session_meta(session)
                        })
                        .collect();
                    let foreign = self.session_store.foreign_session_activity().await?;
                    for session in &mut sessions {
                        if let Some(activity) = foreign.get(&session.id) {
                            session.profile_locked = true;
                            session.is_running = true;
                            session.waiting_for_approval = activity.awaiting_approval;
                            session.is_streaming = activity.streaming;
                        }
                    }
                    Ok(sessions)
                }
                .await;
                respond(response, result);
            }
            Command::ListUserInputHistory { limit, response } => {
                let history = self.agent_manager.list_user_input_history(limit).await;
                respond(response, history);
            }
            Command::GetWorkspaceState {
                session_id,
                response,
            } => {
                let result = self
                    .agent_manager
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
                    .get_tip(&session_id)
                    .await
                    .map(|tip_id| tip_id.map(|id| id.to_string()));
                respond(response, tip_id);
            }
            Command::GetHistoryPage {
                session_id,
                from,
                limit,
                response,
            } => respond(response, self.history_page(&session_id, from, limit).await),
            Command::GetSessionContextUsage {
                session_id,
                response,
            } => {
                let usage = self
                    .agent_manager
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
            Command::GetSessionModel {
                session_id,
                response,
            } => {
                respond(
                    response,
                    self.agent_manager.get_session_model(&session_id).await,
                );
            }
            Command::SetSessionModel {
                session_id,
                selection,
                response,
            } => {
                respond(
                    response,
                    self.agent_manager
                        .set_session_model(&session_id, selection)
                        .await,
                );
            }
            Command::GetOpenAiCodexAuthStatus { response } => {
                let _ = response.send(Ok(self.openai_codex_auth.get_status().await));
            }
            Command::GetMcpAuthStatuses { response } => {
                let mcp = self.agent_manager.mcp();
                let _ = response.send(Ok(mcp.auth_statuses().await));
            }
            Command::StartMcpLogin { server, response } => {
                let mcp = self.agent_manager.mcp();
                respond(response, mcp.start_login(&server).await);
            }
            Command::CancelMcpLogin { server, response } => {
                let mcp = self.agent_manager.mcp();
                respond(response, mcp.cancel_login(&server).await);
            }
            Command::LogoutMcp { server, response } => {
                let mcp = self.agent_manager.mcp();
                respond(response, mcp.logout(&server).await);
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
            Command::Shutdown { .. }
            | Command::SendMessage { .. }
            | Command::ContinueSession { .. }
            | Command::CancelStream { .. }
            | Command::CancelTurn { .. }
            | Command::ApproveScript { .. }
            | Command::DenyScript { .. }
            | Command::GetPendingScript { .. }
            | Command::GetSessionSnapshot { .. }
            | Command::SetSessionMcpServers { .. }
            | Command::UndoLastUserMessage { .. }
            | Command::DeleteSession { .. } => {
                return Err(eyre!("Session command reached global dispatch"));
            }
        }

        Ok(())
    }
}
