use super::*;

impl App {
    pub(super) fn handle_runtime_event(&mut self, event: Event) {
        self.apply_runtime_event(event, true);
    }

    pub(super) fn apply_runtime_event(&mut self, event: Event, apply_state: bool) {
        if !apply_state
            && !matches!(
                &event,
                Event::SessionError { .. }
                    | Event::StreamStart { .. }
                    | Event::ItemStarted { .. }
                    | Event::ItemDelta { .. }
                    | Event::StreamRestarted { .. }
                    | Event::StreamComplete { .. }
                    | Event::StreamError { .. }
                    | Event::ContinuationFailed { .. }
                    | Event::ScriptApprovalRequested { .. }
                    | Event::ScriptStarted { .. }
                    | Event::ScriptOutputTail { .. }
            )
        {
            return;
        }

        match event {
            Event::TurnCompleted { session_id } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.state.running_scripts.clear();
                }
                self.request(RuntimeRequest::GetSessionSnapshot { session_id });
            }
            Event::ScriptPrepared { .. } => {}
            Event::ScriptStarted {
                session_id,
                call_id,
                timeout_millis,
            } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    let started = Instant::now();
                    self.state.running_scripts.insert(
                        call_id,
                        RunningScript {
                            started,
                            timeout: Duration::from_millis(timeout_millis),
                            tail: Vec::new(),
                        },
                    );
                    self.state.script_clock = Some(started);
                    self.invalidate_script_cards();
                }
            }
            Event::ScriptOutputTail {
                session_id,
                call_id,
                lines,
            } => {
                if self.state.current_session_id.as_ref() == Some(&session_id)
                    && let Some(script) = self.state.running_scripts.get_mut(&call_id)
                {
                    script.tail = lines;
                    self.invalidate_script_cards();
                }
            }
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
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    let now = Instant::now();
                    if timer.elapsed(now).is_some() {
                        self.state.feedback.completion_until = None;
                        self.state.turn_cancelled = false;
                    } else if self.state.turn_timer.elapsed(now).is_some()
                        && timer.last_duration().is_some()
                    {
                        self.state.feedback.completion_until = Some(now + Duration::from_secs(1));
                    }
                    self.state.turn_timer = timer;
                }
            }
            Event::ModelsUpdated => {
                if self.startup_sync != StartupSync::WaitingForRuntime {
                    self.request(RuntimeRequest::ListModels);
                }
            }
            Event::ConfigLoaded => {
                self.state.config_loaded = true;
                if self.startup_sync != StartupSync::WaitingForRuntime {
                    self.request_sync();
                }
            }
            Event::ServiceError { error } => {
                self.set_error(format!("Runtime error: {error}"));
            }
            Event::SessionError { session_id, error } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.set_error(format!("Session error: {error}"));
                    self.request_sync_for_session(&session_id);
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
            Event::StreamStart { session_id, .. } => {
                self.mark_session_model_active(&session_id);
                self.request(RuntimeRequest::ListSessions);
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    return;
                }
                if !apply_state {
                    return;
                }
                self.state.is_streaming = true;
                self.state.retry_waiting = false;
                self.state.profile_locked = true;
                self.state.script_phase = ScriptPhase::Idle;
                self.state.profile_lock_stale_after_terminal_event = false;
                self.last_statusline_animation_tick = None;
            }
            Event::ItemStarted {
                session_id,
                message_id,
                item_index,
                item,
            } => {
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    return;
                }
                if apply_state && !self.apply_item_started(&message_id, item_index, item) {
                    self.request_sync_for_session(&session_id);
                }
            }
            Event::ItemDelta {
                session_id,
                message_id,
                item_index,
                delta,
            } => {
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    return;
                }
                if apply_state && !self.apply_item_delta(&message_id, item_index, &delta) {
                    self.request_sync_for_session(&session_id);
                }
            }
            Event::ReasoningDelta { .. } => {}
            Event::MessageUpserted {
                session_id,
                message,
            } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.upsert_message(*message);
                }
            }
            Event::MessagesRemoved {
                session_id,
                message_ids,
            } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.remove_messages(&message_ids);
                }
            }
            Event::SessionStateChanged {
                session_id,
                activity,
                ..
            } => {
                self.request(RuntimeRequest::ListSessions);
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.state.is_streaming = activity == kraai_runtime::SessionActivity::Streaming;
                    self.state.script_phase = match activity {
                        kraai_runtime::SessionActivity::AwaitingApproval => {
                            ScriptPhase::AwaitingApproval
                        }
                        kraai_runtime::SessionActivity::ExecutingScript => ScriptPhase::Executing,
                        kraai_runtime::SessionActivity::Idle
                        | kraai_runtime::SessionActivity::Streaming => ScriptPhase::Idle,
                    };
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
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    if apply_state {
                        self.state.is_streaming = false;
                        self.state.retry_waiting = false;
                        self.last_statusline_animation_tick = None;
                    }
                    self.request_sync_for_session(&session_id);
                }
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    self.request_sync_for_session(&session_id);
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamError {
                session_id, error, ..
            } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    if apply_state {
                        self.state.is_streaming = false;
                        self.state.retry_waiting = false;
                        self.last_statusline_animation_tick = None;
                        self.state.profile_lock_stale_after_terminal_event =
                            self.state.profile_locked;
                    }
                    self.set_error(format!("Stream error: {error}"));
                    self.request_sync_for_session(&session_id);
                }
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    self.request_sync_for_session(&session_id);
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamCancelled { session_id, .. } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.last_statusline_animation_tick = None;
                    self.state.profile_lock_stale_after_terminal_event = self.state.profile_locked;
                    self.state.feedback.completion_until = None;
                    self.state.turn_cancelled = true;
                    self.request_sync_for_session(&session_id);
                }
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    self.request_sync_for_session(&session_id);
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamRestarted {
                session_id,
                message_id,
                ..
            } => {
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    return;
                }
                if apply_state {
                    self.discard_streamed_content(&message_id);
                }
            }
            Event::ProviderRetryScheduled { session_id, .. } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.state.retry_waiting = true;
                    self.state.profile_lock_stale_after_terminal_event = false;
                }
            }
            Event::ContextStateChanged { .. } => {}
            Event::ContinuationFailed { session_id, error } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    if apply_state {
                        self.state.is_streaming = false;
                        self.state.retry_waiting = false;
                        self.last_statusline_animation_tick = None;
                        self.state.profile_lock_stale_after_terminal_event =
                            self.state.profile_locked;
                    }
                    self.set_error(format!("Continuation failed: {error}"));
                    self.request_sync_for_session(&session_id);
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
            Event::HistoryUpdated { session_id } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.clamp_chat_scroll();
                    self.request_sync_for_session(&session_id);
                } else {
                    self.request(RuntimeRequest::GetSessionSnapshot {
                        session_id: session_id.clone(),
                    });
                    self.request(RuntimeRequest::ListSessions);
                }
            }
            Event::ProviderAuthUpdated { status } => {
                let status = map_provider_auth_status(status);
                let settled = pending_auth_target(&status).is_none();
                if self.apply_provider_auth_status(status)
                    && self.state.mode == UiMode::Providers
                    && self.state.providers_view == ProvidersView::Provider
                    && settled
                {
                    self.provider_feedback("Sign-in updated");
                }
            }
            Event::McpAuthUpdated { status } => {
                self.apply_mcp_auth_status(status);
            }
            Event::ScriptApprovalRequested { session_id, script } => {
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    self.request(RuntimeRequest::ListSessions);
                    return;
                }
                if !apply_state {
                    return;
                }
                self.state.pending_script = Some(script);
                self.enter_script_decision_phase();
            }
            Event::ScriptResultReady {
                session_id,
                call_id,
                ..
            } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    if self.state.running_scripts.remove(&call_id).is_some() {
                        self.invalidate_script_cards();
                    }
                    self.state.pending_script = None;
                    self.state.script_phase = ScriptPhase::Idle;
                    self.state.profile_lock_stale_after_terminal_event = false;
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
        }
    }
}
