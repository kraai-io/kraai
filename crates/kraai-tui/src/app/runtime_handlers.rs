use super::*;

impl App {
    pub(super) fn handle_runtime_event(&mut self, event: Event) {
        match event {
            Event::RequestUsageUpdated {
                session_id,
                request,
            } => {
                self.update_costs(
                    &session_id,
                    std::collections::BTreeMap::from([(request.message_id.clone(), *request)]),
                );
            }
            Event::TurnTimingChanged { session_id, timer } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.turn_timer = timer;
                }
            }
            Event::ConfigLoaded => {
                self.state.config_loaded = true;
                self.state.status = String::from("Config loaded");
                self.request_sync();
            }
            Event::ServiceError { error } => {
                self.state.status = format!("Runtime error: {error}");
                self.fail_ci(format!("Runtime error: {error}"));
            }
            Event::SessionError { session_id, error } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.status = format!("Session error: {error}");
                    self.request_sync_for_session(&session_id);
                    self.fail_ci(format!("Session error: {error}"));
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
            Event::StreamStart {
                session_id,
                message_id,
            } => {
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
                self.state.script_phase = ScriptPhase::Idle;
                self.state.profile_lock_stale_after_terminal_event = false;
                self.state.statusline_animation_frame = 0;
                self.last_statusline_animation_tick = None;
                self.last_stream_history_request = None;
                self.stream_event_content
                    .insert(MessageId::new(message_id), String::new());
                self.request_stream_history_sync(&session_id, Instant::now());
            }
            Event::StreamChunk {
                session_id,
                message_id,
                chunk,
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                if self.is_ci_mode() {
                    self.write_ci_output(&chunk);
                    if self.state.exit {
                        return;
                    }
                }
                if !self.append_stream_chunk_to_cached_message(&message_id, &chunk) {
                    self.request_stream_history_sync(&session_id, Instant::now());
                }
            }
            Event::StreamComplete {
                session_id,
                message_id,
            } => {
                let message_id = MessageId::new(message_id);
                self.mark_exit_usage_message_completed(message_id.clone());
                self.request(RuntimeRequest::ListUserInputHistory {
                    limit: INPUT_HISTORY_LIMIT,
                });
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.last_stream_history_request = None;
                    self.stream_event_content.remove(&message_id);
                    self.request_sync_for_session(&session_id);
                    if self.is_ci_mode() {
                        self.finish_ci_output_line();
                        self.ci_turn_completion_pending = true;
                        self.ci_metrics_history_pending = true;
                        self.ci_metrics_context_pending = true;
                    }
                }
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    self.request_sync_for_session(&session_id);
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamError {
                session_id,
                message_id,
                error,
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.last_stream_history_request = None;
                    self.stream_event_content
                        .remove(&MessageId::new(message_id));
                    self.state.profile_lock_stale_after_terminal_event = self.state.profile_locked;
                    self.state.status = format!("Stream error: {error}");
                    self.request_sync_for_session(&session_id);
                }
                self.fail_ci(format!("Stream error: {error}"));
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    self.request_sync_for_session(&session_id);
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamCancelled {
                session_id,
                message_id,
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.last_stream_history_request = None;
                    self.stream_event_content
                        .remove(&MessageId::new(message_id));
                    self.state.profile_lock_stale_after_terminal_event = self.state.profile_locked;
                    self.state.status = String::from("Stream cancelled");
                    self.request_sync_for_session(&session_id);
                }
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    self.request_sync_for_session(&session_id);
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
                    self.state.profile_lock_stale_after_terminal_event = false;
                    self.state.status =
                        format!("Provider error, retry #{retry_number} in {delay_seconds}s");
                }
            }
            Event::ContextStateChanged {
                session_id,
                notifications,
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str())
                    && !notifications.is_empty()
                {
                    self.state.status = notifications.join(" ");
                }
            }
            Event::ContinuationFailed { session_id, error } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.last_stream_history_request = None;
                    self.state.profile_lock_stale_after_terminal_event = self.state.profile_locked;
                    self.state.status = format!("Continuation failed: {error}");
                    self.request_sync_for_session(&session_id);
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
                    self.request(RuntimeRequest::GetSessionSnapshot {
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
            Event::ScriptApprovalRequested { session_id, script } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    self.request(RuntimeRequest::ListSessions);
                    return;
                }
                if self.is_ci_mode() {
                    self.fail_ci(String::from(
                        "CI mode cannot answer a script capability escalation prompt",
                    ));
                    return;
                }
                self.state.pending_script = Some(script);
                self.enter_script_decision_phase();
                self.state.status = String::from("Script capability approval required");
            }
            Event::ScriptResultReady {
                session_id,
                execution_id: _,
                status,
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.pending_script = None;
                    self.state.script_phase = ScriptPhase::Executing;
                    self.state.profile_lock_stale_after_terminal_event = false;
                    self.state.status = format!("Script {status}; waiting for assistant");
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
            RuntimeResponse::AgentProfileCatalog(result) => match result {
                Ok(catalog) => {
                    self.state.agent_profiles = catalog.profiles;
                    self.state.agent_profile_warnings = catalog.warnings;
                    let selected_is_available = self
                        .state
                        .selected_profile_id
                        .as_ref()
                        .is_some_and(|selected| {
                            self.state
                                .agent_profiles
                                .iter()
                                .any(|profile| &profile.id == selected)
                        });
                    if !selected_is_available {
                        self.state.selected_profile_id = Some(catalog.default_profile_id);
                    }
                    self.state.agent_menu_index = self
                        .state
                        .selected_profile_id
                        .as_ref()
                        .and_then(|selected| {
                            self.state
                                .agent_profiles
                                .iter()
                                .position(|profile| &profile.id == selected)
                        })
                        .unwrap_or(0);
                    if let Some(warning) = self.state.agent_profile_warnings.first() {
                        self.state.status = format!("Agent profile warning: {}", warning.message);
                    }
                    self.maybe_send_startup_message();
                }
                Err(error) => {
                    self.state.status = format!("Failed loading agent profiles: {error}");
                    self.fail_ci(format!("Failed loading agent profiles: {error}"));
                }
            },
            RuntimeResponse::ProviderDefinitions(Ok(definitions)) => {
                self.state.provider_definitions = definitions;
            }
            RuntimeResponse::ProviderDefinitions(Err(err)) => {
                self.state.status = format!("Failed loading provider definitions: {err}");
            }
            RuntimeResponse::Settings(Ok(settings)) => {
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
            RuntimeResponse::CreateSession {
                creation_id,
                result: Ok(session_id),
            } => {
                if self
                    .state
                    .pending_submit
                    .as_ref()
                    .is_none_or(|pending| pending.creation_id != creation_id)
                {
                    return;
                }
                let draft_profile_id = self.state.selected_profile_id.clone();
                let pending_submit = self.state.pending_submit.take().map(|mut pending_submit| {
                    pending_submit.session_id = Some(session_id.clone());
                    pending_submit
                });
                self.reset_chat_session(Some(session_id.clone()), "Session ready");
                self.state.pending_submit = pending_submit;
                self.state.selected_profile_id = draft_profile_id.clone();
                self.request_sync_for_session(&session_id);

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
            RuntimeResponse::CreateSession {
                creation_id,
                result: Err(err),
            } => {
                if self
                    .state
                    .pending_submit
                    .as_ref()
                    .is_none_or(|pending| pending.creation_id != creation_id)
                {
                    return;
                }
                self.state.pending_submit = None;
                self.state.status = format!("Failed creating session: {err}");
                self.fail_ci(format!("Failed creating session: {err}"));
            }
            RuntimeResponse::SetSessionProfile {
                session_id,
                profile_id,
                result: Ok(()),
            } => {
                let is_foreground =
                    self.state.current_session_id.as_deref() == Some(session_id.as_str());
                self.request(RuntimeRequest::ListSessions);
                if is_foreground {
                    self.state.selected_profile_id = Some(profile_id.clone());
                    self.state.status = format!("Selected agent: {profile_id}");
                    self.save_workspace_preferences();
                    self.request(RuntimeRequest::GetSessionSnapshot {
                        session_id: session_id.clone(),
                    });
                    self.state.mode = UiMode::Chat;
                }

                if self
                    .state
                    .pending_submit
                    .as_ref()
                    .and_then(|pending| pending.session_id.as_deref())
                    == Some(session_id.as_str())
                    && let Some(pending_submit) = self.state.pending_submit.take()
                {
                    if is_foreground {
                        self.dispatch_send_message(
                            session_id,
                            pending_submit.message,
                            pending_submit.model_id,
                            pending_submit.provider_id,
                            false,
                        );
                    } else {
                        self.request(RuntimeRequest::SendMessage {
                            session_id,
                            message: pending_submit.message,
                            model_id: pending_submit.model_id,
                            provider_id: pending_submit.provider_id,
                        });
                    }
                }
            }
            RuntimeResponse::SetSessionProfile {
                session_id,
                result: Err(err),
                ..
            } => {
                if self
                    .state
                    .pending_submit
                    .as_ref()
                    .and_then(|pending| pending.session_id.as_deref())
                    == Some(session_id.as_str())
                {
                    self.state.pending_submit = None;
                }
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.profile_lock_stale_after_terminal_event = false;
                    self.state.status = format!("Failed changing agent: {err}");
                    self.fail_ci(format!("Failed changing agent: {err}"));
                }
            }
            RuntimeResponse::SendMessage(Ok(_outcome)) => {}
            RuntimeResponse::SendMessage(Err(err)) => {
                if !self.state.optimistic_messages.is_empty() {
                    self.state.optimistic_messages.remove(0);
                    self.update_queued_status();
                    self.invalidate_chat_cache();
                }
                self.state.is_streaming = false;
                self.state.profile_lock_stale_after_terminal_event = false;
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
                self.state.settings_errors = err
                    .violations
                    .iter()
                    .map(|violation| (violation.field.clone(), violation.message.clone()))
                    .collect();
                self.state.status = format!("Failed saving settings: {err}");
            }
            RuntimeResponse::ChatHistory { session_id, result } => {
                match result {
                    Ok(mut history) => {
                        self.merge_local_streaming_content(&mut history);
                        self.accumulate_exit_usage_from_history(&history);
                        if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                            self.state.chat_history = history;
                            self.invalidate_chat_cache();
                            self.reconcile_optimistic_messages();
                            self.clamp_chat_scroll();
                        }
                    }
                    Err(err) => {
                        if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                            self.state.status = format!("Failed loading history: {err}");
                        }
                    }
                }
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.ci_metrics_history_pending = false;
                    self.maybe_finish_ci_run();
                }
            }
            RuntimeResponse::SessionSnapshot { session_id, result } => {
                match *result {
                    Ok(mut snapshot) => {
                        if self
                            .last_session_event_sequences
                            .get(&session_id)
                            .is_some_and(|event_sequence| snapshot.event_sequence < *event_sequence)
                        {
                            if self.state.current_session_id.as_deref() == Some(session_id.as_str())
                            {
                                self.ci_metrics_history_pending = false;
                                self.ci_metrics_context_pending = false;
                                self.maybe_finish_ci_run();
                            }
                            return;
                        }
                        self.session_snapshot_sequences
                            .insert(session_id.clone(), snapshot.event_sequence);
                        self.merge_local_streaming_content(&mut snapshot.history);
                        self.accumulate_exit_usage_from_history(&snapshot.history);
                        self.update_costs(&session_id, snapshot.requests);
                        self.state.cost_recovery_sessions.remove(&session_id);
                        if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                            self.state.turn_timer = snapshot.turn_timer;
                            self.state.current_tip_id = snapshot.session.tip_id.clone();
                            self.state.chat_history = snapshot.history;
                            self.state.context_usage = snapshot.context_usage;
                            self.state.pending_script = snapshot.pending_script;
                            self.apply_agent_profiles_state(snapshot.profiles);
                            self.state.is_streaming = matches!(
                                snapshot.activity,
                                kraai_runtime::SessionActivity::Streaming
                            );
                            self.state.script_phase = match snapshot.activity {
                                kraai_runtime::SessionActivity::AwaitingApproval => {
                                    ScriptPhase::AwaitingApproval
                                }
                                kraai_runtime::SessionActivity::ExecutingScript => {
                                    ScriptPhase::Executing
                                }
                                kraai_runtime::SessionActivity::Idle
                                | kraai_runtime::SessionActivity::Streaming => ScriptPhase::Idle,
                            };
                            self.state.profile_locked = snapshot.session.profile_locked;
                            if let Some(existing) = self
                                .state
                                .sessions
                                .iter_mut()
                                .find(|session| session.id == snapshot.session.id)
                            {
                                *existing = snapshot.session;
                            }
                            self.invalidate_chat_cache();
                            self.reconcile_optimistic_messages();
                            self.clamp_chat_scroll();
                        }
                    }
                    Err(error) => {
                        if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                            self.state.status = format!("Failed loading session: {error}");
                        }
                    }
                }
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.ci_metrics_history_pending = false;
                    self.ci_metrics_context_pending = false;
                    self.maybe_finish_ci_run();
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
                if self.event_lag_session_resync_pending {
                    self.sync_current_session_streaming_from_sessions();
                    self.event_lag_session_resync_pending = false;
                }
                if self.state.cost_recovery_list_pending {
                    self.state.cost_recovery_list_pending = false;
                    self.state.cost_recovery_sessions = self
                        .state
                        .sessions
                        .iter()
                        .map(|session| session.id.clone())
                        .collect();
                    for session_id in self.state.cost_recovery_sessions.clone() {
                        self.request_sync_for_session(&session_id);
                    }
                }
                self.maybe_finish_ci_run();
            }
            RuntimeResponse::Sessions(Err(err)) => {
                self.state.status = format!("Failed loading sessions: {err}");
            }
            RuntimeResponse::UserInputHistory(Ok(history)) => {
                self.state.input_history = history;
                self.reset_input_history_navigation();
            }
            RuntimeResponse::UserInputHistory(Err(err)) => {
                self.state.status = format!("Failed loading input history: {err}");
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
            RuntimeResponse::ApproveScript {
                session_id,
                execution_id,
                result: Ok(()),
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                if self
                    .state
                    .pending_script
                    .as_ref()
                    .is_some_and(|script| script.execution_id == execution_id)
                {
                    self.state.pending_script = None;
                }
                self.state.script_phase = ScriptPhase::Executing;
                self.state.profile_lock_stale_after_terminal_event = false;
                self.state.status = String::from("Executing approved Nushell script");
            }
            RuntimeResponse::ApproveScript {
                session_id,
                result: Err(err),
                ..
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.status = format!("Failed approving script: {err}");
                }
            }
            RuntimeResponse::DenyScript {
                session_id,
                execution_id,
                result: Ok(()),
            } => {
                if self.state.current_session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                if self
                    .state
                    .pending_script
                    .as_ref()
                    .is_some_and(|script| script.execution_id == execution_id)
                {
                    self.state.pending_script = None;
                }
                self.state.script_phase = ScriptPhase::Executing;
                self.state.status = String::from("Script capability escalation denied");
            }
            RuntimeResponse::DenyScript {
                session_id,
                result: Err(err),
                ..
            } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.state.status = format!("Failed denying script: {err}");
                }
            }
            RuntimeResponse::CancelStream(Ok(true)) => {}
            RuntimeResponse::CancelStream(Ok(false)) => {
                self.state.status = String::from("No active stream to cancel");
            }
            RuntimeResponse::CancelStream(Err(err)) => {
                self.state.status = format!("Failed cancelling stream: {err}");
            }
            RuntimeResponse::ContinueSession(Ok(outcome)) => match outcome {
                kraai_runtime::ContinueSessionOutcome::Started => {
                    self.state.script_phase = ScriptPhase::Idle;
                    self.state.profile_lock_stale_after_terminal_event = false;
                    self.state.status = String::from("Continuing session");
                }
                kraai_runtime::ContinueSessionOutcome::NothingToContinue => {
                    self.state.status = String::from("Nothing to continue");
                }
            },
            RuntimeResponse::ContinueSession(Err(err)) => {
                self.state.status = format!("Failed continuing session: {err}");
            }
        }
    }
}
