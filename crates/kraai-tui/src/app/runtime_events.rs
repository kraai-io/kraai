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
                if self.startup_sync != StartupSync::WaitingForRuntime {
                    self.request_sync();
                }
            }
            Event::ServiceError { error } => {
                self.set_error(format!("Runtime error: {error}"));
                self.fail_ci(format!("Runtime error: {error}"));
            }
            Event::SessionError { session_id, error } => {
                if self.state.current_session_id.as_deref() == Some(session_id.as_str()) {
                    self.set_error(format!("Session error: {error}"));
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
                    self.set_error(format!("Stream error: {error}"));
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
                    self.set_error(format!("Continuation failed: {error}"));
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
                    self.state.script_phase = ScriptPhase::Idle;
                    self.state.profile_lock_stale_after_terminal_event = false;
                    self.state.status = format!("Script {status}; waiting for assistant");
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
        }
    }
}
