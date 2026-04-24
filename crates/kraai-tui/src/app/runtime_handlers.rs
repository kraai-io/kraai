use super::*;

impl App {
    pub(super) fn handle_runtime_event(&mut self, event: Event) {
        match event {
            Event::ConfigLoaded => {
                self.state.config_loaded = true;
                self.state.status = String::from("Config loaded");
                self.request_sync();
            }
            Event::Error(msg) => {
                self.state.is_streaming = false;
                self.state.retry_waiting = false;
                self.state.status = format!("Runtime error: {msg}");
                self.fail_ci(format!("Runtime error: {msg}"));
            }
            Event::StreamStart { session_id, .. } => {
                self.request(RuntimeRequest::ListSessions);
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                if self.is_ci_mode() {
                    self.ci_output_needs_newline = false;
                    self.ci_turn_completion_pending = false;
                }
                self.state.is_streaming = true;
                self.state.retry_waiting = false;
                self.state.profile_locked = true;
                self.state.statusline_animation_frame = 0;
                self.last_statusline_animation_tick = None;
                self.last_stream_refresh = None;
                self.request(RuntimeRequest::GetCurrentTip { session_id });
            }
            Event::StreamChunk {
                session_id, chunk, ..
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                if self.is_ci_mode() {
                    self.write_ci_output(&chunk);
                }
                let now = Instant::now();
                let should_refresh = self
                    .last_stream_refresh
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(50));
                if should_refresh {
                    self.last_stream_refresh = Some(now);
                    self.request(RuntimeRequest::GetCurrentTip {
                        session_id: session_id.clone(),
                    });
                    self.request(RuntimeRequest::GetChatHistory { session_id });
                }
            }
            Event::StreamComplete {
                session_id,
                message_id,
            } => {
                self.mark_exit_usage_message_completed(kraai_types::MessageId::new(message_id));
                self.request(RuntimeRequest::GetChatHistory {
                    session_id: session_id.clone(),
                });
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.last_stream_refresh = None;
                    self.request_sync_for_session(&session_id);
                    if self.state.tool_phase == ToolPhase::ExecutingBatch
                        && self.state.pending_tools.is_empty()
                    {
                        self.finish_tool_batch_execution();
                    }
                    if self.is_ci_mode() {
                        self.finish_ci_output_line();
                        self.ci_turn_completion_pending = true;
                    }
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamError {
                session_id, error, ..
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.last_stream_refresh = None;
                    self.state.status = format!("Stream error: {error}");
                    if self.state.tool_phase == ToolPhase::ExecutingBatch
                        && self.state.pending_tools.is_empty()
                    {
                        self.finish_tool_batch_execution();
                    }
                }
                self.fail_ci(format!("Stream error: {error}"));
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamCancelled { session_id, .. } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.last_stream_refresh = None;
                    self.state.status = String::from("Stream cancelled");
                    self.request_sync_for_session(&session_id);
                    if self.state.tool_phase == ToolPhase::ExecutingBatch
                        && self.state.pending_tools.is_empty()
                    {
                        self.finish_tool_batch_execution();
                    }
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::ProviderRetryScheduled {
                session_id,
                retry_number,
                delay_seconds,
                ..
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.retry_waiting = true;
                    self.state.status =
                        format!("Provider error, retry #{retry_number} in {delay_seconds}s");
                }
            }
            Event::ContinuationFailed { session_id, error } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.last_stream_refresh = None;
                    self.state.status = format!("Continuation failed: {error}");
                    self.request_sync_for_session(&session_id);
                    if self.state.tool_phase == ToolPhase::ExecutingBatch
                        && self.state.pending_tools.is_empty()
                    {
                        self.finish_tool_batch_execution();
                    }
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
                self.fail_ci(format!("Continuation failed: {error}"));
            }
            Event::HistoryUpdated { session_id } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.clamp_chat_scroll();
                    self.request_sync_for_session(&session_id);
                } else {
                    self.request(RuntimeRequest::GetChatHistory {
                        session_id: session_id.clone(),
                    });
                    self.request(RuntimeRequest::ListSessions);
                }
            }
            Event::OpenAiCodexAuthUpdated { status } => {
                self.apply_openai_codex_auth_status(map_openai_codex_auth_status(status));
                if self.state.mode == UiMode::ProvidersMenu
                    && matches!(self.state.providers_view, ProvidersView::Detail)
                    && pending_auth_target(&self.state.openai_codex_auth).is_none()
                {
                    self.state.status = String::from("OpenAI auth updated");
                }
            }
            Event::MessageComplete(_) => {}
            Event::ToolCallDetected {
                session_id,
                call_id,
                tool_id,
                args,
                description,
                risk_level,
                reasons,
                queue_order,
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    self.request(RuntimeRequest::ListSessions);
                    return;
                }

                let exists = self
                    .state
                    .pending_tools
                    .iter()
                    .any(|tool| tool.call_id == call_id);
                if !exists {
                    if self.is_ci_mode() {
                        self.fail_ci(format!("CI mode does not support tool approval: {tool_id}"));
                        return;
                    }
                    self.state.pending_tools.push(PendingTool {
                        call_id,
                        tool_id,
                        args,
                        description,
                        risk_level,
                        reasons,
                        approved: None,
                        queue_order,
                    });
                }
                self.sort_pending_tools();
                self.enter_tool_decision_phase();
                self.state.status =
                    format!("{} tool call(s) pending", self.state.pending_tools.len());
            }
            Event::ToolResultReady {
                session_id,
                call_id,
                tool_id,
                success,
                denied,
                output,
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state
                        .pending_tools
                        .retain(|tool| tool.call_id != call_id);
                    self.sort_pending_tools();
                    if !success || denied {
                        self.push_optimistic_tool_message(&call_id, &tool_id, &output, denied);
                    }
                    self.state.status = if denied {
                        format!("Tool denied: {tool_id}")
                    } else if success {
                        format!("Tool succeeded: {tool_id}")
                    } else {
                        format!("Tool failed: {tool_id}")
                    };
                    if self.state.pending_tools.is_empty()
                        && self.state.tool_phase == ToolPhase::ExecutingBatch
                        && !self.state.is_streaming
                    {
                        self.state.status = format!("Waiting for assistant after {tool_id}");
                    } else {
                        self.sync_tool_phase_from_pending_tools();
                    }
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
        }
    }

    pub(super) fn handle_runtime_response(&mut self, response: RuntimeResponse) {
        match response {
            RuntimeResponse::Models(Ok(models)) => {
                self.state.config_loaded = true;
                self.state.models_by_provider = models;
                if self.is_ci_mode() {
                    if let Err(err) = self.validate_ci_model_selection() {
                        self.fail_ci(err);
                        return;
                    }
                } else {
                    self.ensure_selected_model();
                }
                self.maybe_send_startup_message();
            }
            RuntimeResponse::Models(Err(err)) => {
                self.state.status = format!("Failed loading models: {err}");
                self.fail_ci(format!("Failed loading models: {err}"));
            }
            RuntimeResponse::AgentProfiles {
                session_id,
                result: Ok(state),
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                self.apply_agent_profiles_state(state);
                self.maybe_finish_ci_run();
            }
            RuntimeResponse::AgentProfiles {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed loading agent profiles: {err}");
            }
            RuntimeResponse::ProviderDefinitions(Ok(definitions)) => {
                self.state.provider_definitions = definitions;
            }
            RuntimeResponse::ProviderDefinitions(Err(err)) => {
                self.state.status = format!("Failed loading provider definitions: {err}");
            }
            RuntimeResponse::Settings(Ok(settings)) => {
                self.clear_chat_selection();
                self.state.visible_chat_view = None;
                self.state.settings_draft = Some(settings);
                self.state.settings_errors.clear();
                self.state.settings_focus = SettingsFocus::ProviderList;
                self.state.settings_provider_index = 0;
                self.state.settings_model_index = 0;
                self.state.settings_provider_field_index = 0;
                self.state.settings_model_field_index = 0;
                self.state.settings_editor = None;
                self.state.settings_editor_input.clear();
                self.state.settings_delete_armed = false;
                self.state.providers_view = ProvidersView::List;
                self.state.providers_advanced_focus = ProvidersAdvancedFocus::ProviderFields;
                self.state.connect_provider_search.clear();
                self.state.connect_provider_index = 0;
                self.state.mode = UiMode::ProvidersMenu;
                self.state.status = String::from("Providers loaded");
            }
            RuntimeResponse::Settings(Err(err)) => {
                self.state.status = format!("Failed loading settings: {err}");
            }
            RuntimeResponse::OpenAiCodexAuthStatus(result)
            | RuntimeResponse::StartOpenAiCodexBrowserLogin(result)
            | RuntimeResponse::StartOpenAiCodexDeviceCodeLogin(result)
            | RuntimeResponse::CancelOpenAiCodexLogin(result)
            | RuntimeResponse::LogoutOpenAiCodexAuth(result) => match result {
                Ok(status) => {
                    self.apply_openai_codex_auth_status(status);
                }
                Err(err) => {
                    self.state.status = format!("OpenAI auth failed: {err}");
                }
            },
            RuntimeResponse::CreateSession(Ok(session_id)) => {
                let draft_profile_id = self.state.selected_profile_id.clone();
                let pending_submit = self.state.pending_submit.take().map(|mut pending_submit| {
                    pending_submit.session_id = Some(session_id.clone());
                    pending_submit
                });
                self.reset_chat_session(Some(session_id.clone()), "Session ready");
                self.state.pending_submit = pending_submit;
                self.state.selected_profile_id = draft_profile_id.clone();
                self.request_sync_for_session(&session_id);

                if draft_profile_id.as_deref() != Some(DEFAULT_AGENT_PROFILE_ID)
                    && let Some(profile_id) = draft_profile_id
                {
                    self.request(RuntimeRequest::SetSessionProfile {
                        session_id,
                        profile_id,
                    });
                    return;
                }

                if let Some(pending_submit) = self.state.pending_submit.take() {
                    self.dispatch_send_message(
                        session_id,
                        pending_submit.message,
                        pending_submit.model_id,
                        pending_submit.provider_id,
                        false,
                    );
                }
            }
            RuntimeResponse::CreateSession(Err(err)) => {
                self.state.pending_submit = None;
                self.state.status = format!("Failed creating session: {err}");
                self.fail_ci(format!("Failed creating session: {err}"));
            }
            RuntimeResponse::SetSessionProfile {
                profile_id,
                result: Ok(()),
            } => {
                self.state.selected_profile_id = Some(profile_id.clone());
                self.state.status = format!("Selected agent: {profile_id}");
                if let Some(session_id) = self.state.current_session_id.clone() {
                    self.request(RuntimeRequest::ListSessions);
                    self.request(RuntimeRequest::ListAgentProfiles { session_id });
                }
                self.state.mode = UiMode::Chat;

                if let Some(pending_submit) = self.state.pending_submit.take()
                    && let Some(session_id) = pending_submit.session_id
                {
                    self.dispatch_send_message(
                        session_id,
                        pending_submit.message,
                        pending_submit.model_id,
                        pending_submit.provider_id,
                        false,
                    );
                }
            }
            RuntimeResponse::SetSessionProfile {
                result: Err(err), ..
            } => {
                self.state.pending_submit = None;
                self.state.status = format!("Failed changing agent: {err}");
                self.fail_ci(format!("Failed changing agent: {err}"));
            }
            RuntimeResponse::SendMessage(Ok(())) => {}
            RuntimeResponse::SendMessage(Err(err)) => {
                if !self.state.optimistic_messages.is_empty() {
                    self.state.optimistic_messages.remove(0);
                    self.update_queued_status();
                    self.invalidate_chat_cache();
                }
                self.state.is_streaming = false;
                self.state.status = format!("Send failed: {err}");
                self.fail_ci(format!("Send failed: {err}"));
            }
            RuntimeResponse::SaveSettings(Ok(())) => {
                self.state.settings_errors.clear();
                self.state.settings_delete_armed = false;
                self.state.settings_editor = None;
                self.state.settings_editor_input.clear();
                self.state.status = String::from("Providers saved");
                self.request(RuntimeRequest::ListModels);
            }
            RuntimeResponse::SaveSettings(Err(err)) => {
                self.state.settings_errors = parse_settings_errors(&err);
                self.state.status = format!("Failed saving settings: {err}");
            }
            RuntimeResponse::ChatHistory { session_id, result } => match result {
                Ok(history) => {
                    self.accumulate_exit_usage_from_history(&history);
                    if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                        self.state.chat_history = history;
                        self.invalidate_chat_cache();
                        self.reconcile_optimistic_messages();
                        self.reconcile_optimistic_tool_messages();
                        self.clamp_chat_scroll();
                    }
                }
                Err(err) => {
                    if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                        self.state.status = format!("Failed loading history: {err}");
                    }
                }
            },
            RuntimeResponse::SessionContextUsage { session_id, result } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }

                match result {
                    Ok(usage) => {
                        self.state.context_usage = usage;
                    }
                    Err(err) => {
                        self.state.status = format!("Failed loading context usage: {err}");
                    }
                }
            }
            RuntimeResponse::CurrentTip { session_id, result } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }

                match result {
                    Ok(tip) => {
                        if self.state.current_tip_id != tip {
                            self.state.current_tip_id = tip;
                            self.invalidate_chat_cache();
                            self.reconcile_optimistic_messages();
                            self.reconcile_optimistic_tool_messages();
                            self.clamp_chat_scroll();
                        }
                    }
                    Err(err) => {
                        self.state.status = format!("Failed loading tip: {err}");
                    }
                }
            }
            RuntimeResponse::UndoLastUserMessage { session_id, result } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }

                match result {
                    Ok(Some(message)) => {
                        self.set_input_text(message);
                        self.state.status = String::from("Restored last user message");
                        self.request_sync_for_session(&session_id);
                    }
                    Ok(None) => {
                        self.state.status = String::from("No user message to undo");
                    }
                    Err(err) => {
                        self.state.status = format!("Failed to undo: {err}");
                    }
                }
            }
            RuntimeResponse::PendingTools { session_id, result } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }

                match result {
                    Ok(pending_tools) => {
                        let should_auto_start_execution = self.state.tool_phase == ToolPhase::Idle;
                        self.state.pending_tools = pending_tools
                            .into_iter()
                            .map(|tool| PendingTool {
                                call_id: tool.call_id,
                                tool_id: tool.tool_id,
                                args: tool.args,
                                description: tool.description,
                                risk_level: tool.risk_level,
                                reasons: tool.reasons,
                                approved: tool.approved,
                                queue_order: tool.queue_order,
                            })
                            .collect();
                        self.sync_tool_phase_from_pending_tools();
                        if should_auto_start_execution
                            && self.state.tool_phase == ToolPhase::ExecutingBatch
                            && !self.state.tool_batch_execution_started
                            && !self.has_undecided_tools()
                            && !self.state.pending_tools.is_empty()
                        {
                            self.maybe_start_tool_batch_execution();
                        }
                        self.maybe_finish_ci_run();
                    }
                    Err(err) => {
                        self.state.status = format!("Failed loading pending tools: {err}");
                    }
                }
            }
            RuntimeResponse::LoadSession {
                session_id,
                result: Ok(true),
            } => {
                self.reset_chat_session(Some(session_id), "Session loaded");
                self.request_sync();
            }
            RuntimeResponse::LoadSession {
                result: Ok(false), ..
            } => {
                self.state.status = String::from("Session not found");
            }
            RuntimeResponse::LoadSession {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed to load session: {err}");
            }
            RuntimeResponse::Sessions(Ok(sessions)) => {
                self.state.sessions = sessions;
                if self.state.sessions_menu_index > self.state.sessions.len() {
                    self.state.sessions_menu_index = self.state.sessions.len();
                }
                self.sync_current_session_profile_from_sessions();
                self.maybe_finish_ci_run();
            }
            RuntimeResponse::Sessions(Err(err)) => {
                self.state.status = format!("Failed loading sessions: {err}");
            }
            RuntimeResponse::DeleteSession {
                session_id,
                result: Ok(()),
            } => {
                self.state.status = String::from("Session deleted");
                self.state.sessions.retain(|s| s.id != session_id);
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.reset_chat_session(None, "Session deleted");
                }
            }
            RuntimeResponse::DeleteSession {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed deleting session: {err}");
            }
            RuntimeResponse::ApproveTool {
                call_id,
                result: Ok(()),
            } => {
                self.set_tool_approval(&call_id, Some(true));
                if self.has_undecided_tools() {
                    self.enter_tool_decision_phase();
                } else {
                    self.state.tool_phase = ToolPhase::ExecutingBatch;
                    self.maybe_start_tool_batch_execution();
                }
            }
            RuntimeResponse::ApproveTool {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed approving tool: {err}");
            }
            RuntimeResponse::DenyTool {
                call_id,
                result: Ok(()),
            } => {
                self.set_tool_approval(&call_id, Some(false));
                if self.has_undecided_tools() {
                    self.enter_tool_decision_phase();
                } else {
                    self.state.tool_phase = ToolPhase::ExecutingBatch;
                    self.maybe_start_tool_batch_execution();
                }
            }
            RuntimeResponse::DenyTool {
                result: Err(err), ..
            } => {
                self.state.status = format!("Failed denying tool: {err}");
            }
            RuntimeResponse::CancelStream(Ok(true)) => {}
            RuntimeResponse::CancelStream(Ok(false)) => {
                self.state.status = String::from("No active stream to cancel");
            }
            RuntimeResponse::CancelStream(Err(err)) => {
                self.state.status = format!("Failed cancelling stream: {err}");
            }
            RuntimeResponse::ContinueSession(Ok(())) => {
                self.state.status = String::from("Continuing session");
            }
            RuntimeResponse::ContinueSession(Err(err)) => {
                self.state.status = format!("Failed continuing session: {err}");
            }
            RuntimeResponse::ExecuteApprovedTools(Ok(())) => {
                self.state.status = String::from("Executing decided tool calls");
            }
            RuntimeResponse::ExecuteApprovedTools(Err(err)) => {
                self.state.tool_batch_execution_started = false;
                self.state.status = format!("Failed executing tools: {err}");
            }
        }
    }
}
