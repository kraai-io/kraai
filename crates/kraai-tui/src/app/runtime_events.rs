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
            )
        {
            return;
        }

        match event {
            Event::TurnCompleted { session_id } => {
                self.request(RuntimeRequest::GetSessionSnapshot { session_id });
            }
            Event::ScriptPrepared { .. } | Event::ScriptStarted { .. } => {}
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
                if self.state.current_session_id.as_ref() == Some(&session_id) {
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
                self.mark_session_model_active(&session_id);
                self.request(RuntimeRequest::ListSessions);
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    return;
                }
                if self.is_ci_mode() {
                    self.ci_output_needs_newline = false;
                    self.ci_turn_completion_pending = false;
                }
                if !apply_state {
                    return;
                }
                self.state.is_streaming = true;
                self.state.retry_waiting = false;
                self.state.profile_locked = true;
                self.state.script_phase = ScriptPhase::Idle;
                self.state.profile_lock_stale_after_terminal_event = false;
                self.state.statusline_animation_frame = 0;
                self.last_statusline_animation_tick = None;
                self.ci_shown_items.remove(&message_id);
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
                if self.is_ci_mode() {
                    let output = match &item {
                        AssistantItem::Text { text, .. } => {
                            self.ci_text(&message_id, item_index, text)
                        }
                        AssistantItem::ScriptCall { input, .. } => {
                            self.ci_shown_items
                                .entry(message_id.clone())
                                .or_default()
                                .insert(item_index);
                            format!("\n\n```nu\n{input}\n```\n")
                        }
                        AssistantItem::Reasoning { .. } => String::new(),
                    };
                    self.write_ci_output(&output);
                    if self.state.exit {
                        return;
                    }
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
                if self.is_ci_mode() {
                    let output = self.ci_text(&message_id, item_index, &delta);
                    self.write_ci_output(&output);
                    if self.state.exit {
                        return;
                    }
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
                        self.state.statusline_animation_frame = 0;
                        self.last_statusline_animation_tick = None;
                    }
                    self.request_sync_for_session(&session_id);
                    if self.is_ci_mode() {
                        self.finish_ci_output_line();
                        self.ci_turn_completion_pending = true;
                        self.ci_metrics_history_pending = true;
                        self.ci_metrics_context_pending = true;
                    }
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
                        self.state.statusline_animation_frame = 0;
                        self.last_statusline_animation_tick = None;
                        self.state.profile_lock_stale_after_terminal_event =
                            self.state.profile_locked;
                    }
                    self.set_error(format!("Stream error: {error}"));
                    self.request_sync_for_session(&session_id);
                }
                self.fail_ci(format!("Stream error: {error}"));
                if self.state.current_session_id.as_ref() != Some(&session_id) {
                    self.request_sync_for_session(&session_id);
                }
                self.request(RuntimeRequest::ListSessions);
            }
            Event::StreamCancelled { session_id, .. } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.state.is_streaming = false;
                    self.state.retry_waiting = false;
                    self.state.statusline_animation_frame = 0;
                    self.last_statusline_animation_tick = None;
                    self.state.profile_lock_stale_after_terminal_event = self.state.profile_locked;
                    self.state.feedback.completion_until = None;
                    self.state.status = String::from("Stream cancelled");
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
                if self.is_ci_mode() {
                    self.write_ci_output("\n\n[provider stream failed; retrying the response]\n\n");
                }
                if apply_state {
                    self.discard_streamed_content(&message_id);
                    self.state.status =
                        String::from("Provider stream failed, retrying the response");
                }
            }
            Event::ProviderRetryScheduled {
                session_id,
                retry_number,
                delay_seconds,
                ..
            } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
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
                if self.is_ci_mode() {
                    for notification in &notifications {
                        if let Err(error) = std::io::Write::write_fmt(
                            &mut std::io::stderr(),
                            format_args!("{notification}\n"),
                        ) {
                            tracing::warn!(%error, "Unable to write context notification");
                        }
                    }
                }
                if self.state.current_session_id.as_ref() == Some(&session_id)
                    && !notifications.is_empty()
                {
                    self.state.status = notifications.join(" ");
                }
            }
            Event::ContinuationFailed { session_id, error } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    if apply_state {
                        self.state.is_streaming = false;
                        self.state.retry_waiting = false;
                        self.state.statusline_animation_frame = 0;
                        self.last_statusline_animation_tick = None;
                        self.state.profile_lock_stale_after_terminal_event =
                            self.state.profile_locked;
                    }
                    self.set_error(format!("Continuation failed: {error}"));
                    self.request_sync_for_session(&session_id);
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
                self.fail_ci(format!("Continuation failed: {error}"));
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
                    && self.state.mode == UiMode::ProvidersMenu
                    && matches!(self.state.providers_view, ProvidersView::Detail)
                    && settled
                {
                    self.state.status = String::from("Provider sign-in updated");
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
                if self.is_ci_mode() {
                    self.fail_ci(String::from(
                        "CI mode cannot answer a script capability escalation prompt",
                    ));
                    return;
                }
                if !apply_state {
                    return;
                }
                self.state.pending_script = Some(script);
                self.enter_script_decision_phase();
                self.state.status = String::from("Script capability approval required");
            }
            Event::ScriptResultReady {
                session_id,
                execution_id: _,
                outcome,
                ..
            } => {
                if self.state.current_session_id.as_ref() == Some(&session_id) {
                    self.state.pending_script = None;
                    self.state.script_phase = ScriptPhase::Idle;
                    self.state.profile_lock_stale_after_terminal_event = false;
                    self.state.status =
                        format!("Script {}; waiting for assistant", outcome.status.as_str());
                } else {
                    self.request(RuntimeRequest::ListSessions);
                }
            }
        }
    }
}
