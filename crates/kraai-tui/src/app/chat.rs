use super::*;

impl App {
    pub(super) fn copy_text_to_clipboard(&mut self, text: &str) -> Result<(), String> {
        let mut errors = Vec::new();
        let mut copied = false;

        match copy_via_osc52(text) {
            Ok(()) => copied = true,
            Err(err) => errors.push(format!("terminal clipboard failed: {err}")),
        }

        match self.clipboard_mut() {
            Ok(clipboard) => match clipboard.set_text(text.to_string()) {
                Ok(()) => copied = true,
                Err(err) => errors.push(format!("clipboard write failed: {err}")),
            },
            Err(err) => errors.push(err),
        }

        if copied {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub(super) fn clipboard_mut(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.clipboard.is_none() {
            self.clipboard = Some(
                arboard::Clipboard::new().map_err(|err| format!("clipboard unavailable: {err}"))?,
            );
        }

        self.clipboard
            .as_mut()
            .ok_or_else(|| String::from("clipboard unavailable"))
    }

    pub(super) fn insert_input_char(&mut self, ch: char) {
        self.reset_input_history_navigation();
        let cursor = self.state.input_cursor.min(self.state.input.len());
        if self.state.input.is_char_boundary(cursor) {
            self.state.input.insert(cursor, ch);
            self.state.input_cursor = cursor + ch.len_utf8();
        }
    }

    pub(super) fn insert_input_text(&mut self, text: &str) {
        self.reset_input_history_navigation();
        let cursor = self.state.input_cursor.min(self.state.input.len());
        if self.state.input.is_char_boundary(cursor) {
            self.state.input.insert_str(cursor, text);
            self.state.input_cursor = cursor + text.len();
        }
    }

    pub(super) fn backspace_input_char(&mut self) {
        self.reset_input_history_navigation();
        let cursor = self.state.input_cursor.min(self.state.input.len());
        if cursor == 0 || !self.state.input.is_char_boundary(cursor) {
            return;
        }

        let prev = self
            .state
            .input
            .char_indices()
            .take_while(|(idx, _)| *idx < cursor)
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.state.input.drain(prev..cursor);
        self.state.input_cursor = prev;
    }

    pub(super) fn move_input_cursor_left(&mut self) {
        let cursor = self.state.input_cursor.min(self.state.input.len());
        let prev = self
            .state
            .input
            .char_indices()
            .take_while(|(idx, _)| *idx < cursor)
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.state.input_cursor = prev;
    }

    pub(super) fn move_input_cursor_right(&mut self) {
        let cursor = self.state.input_cursor.min(self.state.input.len());
        if cursor >= self.state.input.len() {
            self.state.input_cursor = self.state.input.len();
            return;
        }

        let next = self
            .state
            .input
            .char_indices()
            .map(|(idx, _)| idx)
            .find(|idx| *idx > cursor)
            .unwrap_or(self.state.input.len());
        self.state.input_cursor = next;
    }

    pub(super) fn reset_input_history_navigation(&mut self) {
        self.state.input_history_index = None;
        self.state.input_history_draft = None;
    }

    pub(super) fn handle_input_up(&mut self) {
        let nav = TextInput::cursor_navigation(
            &self.state.input,
            self.state.input_cursor,
            self.state.input_width,
        );
        if nav.can_move_up {
            self.state.input_cursor = nav.cursor_above;
            return;
        }

        self.recall_older_input_history();
    }

    pub(super) fn handle_input_down(&mut self) {
        let nav = TextInput::cursor_navigation(
            &self.state.input,
            self.state.input_cursor,
            self.state.input_width,
        );
        if nav.can_move_down {
            self.state.input_cursor = nav.cursor_below;
            return;
        }

        self.recall_newer_input_history();
    }

    fn recall_older_input_history(&mut self) {
        if self.state.input_history.is_empty() {
            return;
        }

        let next_index = match self.state.input_history_index {
            Some(index) => (index + 1).min(self.state.input_history.len().saturating_sub(1)),
            None => {
                self.state.input_history_draft = Some(self.state.input.clone());
                0
            }
        };
        self.apply_input_history_index(next_index);
    }

    fn recall_newer_input_history(&mut self) {
        let Some(index) = self.state.input_history_index else {
            return;
        };

        if index == 0 {
            let draft = self.state.input_history_draft.take().unwrap_or_default();
            self.state.input = draft;
            self.state.input_cursor = self.state.input.len();
            self.state.input_history_index = None;
            return;
        }

        self.apply_input_history_index(index - 1);
    }

    fn apply_input_history_index(&mut self, index: usize) {
        let Some(message) = self.state.input_history.get(index).cloned() else {
            return;
        };
        self.state.input = message;
        self.state.input_cursor = self.state.input.len();
        self.state.input_history_index = Some(index);
    }

    pub(super) fn select_previous_script_action(&mut self) {
        self.state.script_approval_action = match self.state.script_approval_action {
            ScriptApprovalAction::Allow => ScriptApprovalAction::Reject,
            ScriptApprovalAction::Reject => ScriptApprovalAction::Allow,
        };
    }

    pub(super) fn select_next_script_action(&mut self) {
        self.select_previous_script_action();
    }

    pub(super) fn confirm_current_script_action(&mut self) {
        let approved = matches!(
            self.state.script_approval_action,
            ScriptApprovalAction::Allow
        );
        self.submit_script_decision(approved);
    }

    pub(super) fn submit_script_decision(&mut self, approved: bool) {
        let Some(execution_id) = self
            .state
            .pending_script
            .as_ref()
            .map(|script| script.execution_id.clone())
        else {
            return;
        };
        let Some(session_id) = self.state.current_session_id.clone() else {
            return;
        };

        if approved {
            self.request(RuntimeRequest::ApproveScript {
                session_id,
                execution_id,
            });
        } else {
            self.request(RuntimeRequest::DenyScript {
                session_id,
                execution_id,
            });
        }
    }

    pub(super) fn enter_script_decision_phase(&mut self) {
        self.state.mode = UiMode::Chat;
        self.state.script_phase = ScriptPhase::AwaitingApproval;
        self.state.script_approval_action = ScriptApprovalAction::Allow;
        self.pause_turn_timer(Instant::now());
    }

    pub(super) fn request_sync(&mut self) {
        self.request(RuntimeRequest::ListModels);
        self.request(RuntimeRequest::ListSessions);
        self.request(RuntimeRequest::ListUserInputHistory {
            limit: INPUT_HISTORY_LIMIT,
        });
        if let Some(session_id) = self.state.current_session_id.clone() {
            self.request_sync_for_session(&session_id);
        }
    }

    pub(super) fn request_sync_for_session(&mut self, session_id: &str) {
        self.request(RuntimeRequest::GetCurrentTip {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetChatHistory {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetSessionContextUsage {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetPendingScript {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::ListAgentProfiles {
            session_id: session_id.to_string(),
        });
    }

    pub(super) fn reset_chat_session(&mut self, session_id: Option<String>, status: &str) {
        let has_session = session_id.is_some();
        self.state.mode = UiMode::Chat;
        self.state.current_session_id = session_id;
        self.state.current_tip_id = None;
        self.state.chat_history.clear();
        self.state.context_usage = None;
        self.state.optimistic_messages.clear();
        self.stream_event_content.clear();
        self.state.pending_script = None;
        self.state.agent_profiles = if has_session {
            Vec::new()
        } else {
            default_agent_profiles()
        };
        self.state.agent_profile_warnings.clear();
        if has_session {
            self.state.selected_profile_id = None;
        } else {
            self.state
                .selected_profile_id
                .get_or_insert_with(|| String::from(DEFAULT_AGENT_PROFILE_ID));
        }
        self.state.profile_locked = false;
        self.state.profile_lock_stale_after_terminal_event = false;
        self.state.script_approval_action = ScriptApprovalAction::Allow;
        self.state.script_phase = ScriptPhase::Idle;
        self.state.is_streaming = false;
        self.state.retry_waiting = false;
        self.clear_turn_timer();
        self.state.statusline_animation_frame = 0;
        self.last_statusline_animation_tick = None;
        self.last_stream_history_request = None;
        self.state.auto_scroll = true;
        self.state.scroll = 0;
        self.state.status = status.to_string();
        self.invalidate_chat_cache();
        self.clamp_chat_scroll();
    }

    pub(super) fn start_new_chat(&mut self) {
        self.state.pending_submit = None;
        self.reset_chat_session(None, "Started new chat");
    }

    pub(super) fn dispatch_send_message(
        &mut self,
        session_id: String,
        message: String,
        model_id: String,
        provider_id: String,
        is_queued: bool,
    ) {
        if self.request(RuntimeRequest::SendMessage {
            session_id,
            message: message.clone(),
            model_id: model_id.clone(),
            provider_id: provider_id.clone(),
        }) == RuntimeRequestDelivery::Disconnected
        {
            self.set_input_text(message);
            return;
        }

        let content_key = message.trim().to_string();
        let visible_count = self.visible_user_message_count(&content_key);
        let optimistic_same_count = self
            .state
            .optimistic_messages
            .iter()
            .filter(|optimistic| optimistic.content_key == content_key)
            .count();

        self.state.optimistic_seq = self.state.optimistic_seq.saturating_add(1);
        self.state.optimistic_messages.push(OptimisticMessage {
            local_id: format!("local-user-{}", self.state.optimistic_seq),
            content: message.clone(),
            content_key,
            occurrence: visible_count + optimistic_same_count + 1,
            is_queued,
        });

        if is_queued {
            self.update_queued_status();
        } else {
            self.state.script_phase = ScriptPhase::Idle;
            self.start_turn_timer(Instant::now());
            self.state.is_streaming = true;
            self.state.statusline_animation_frame = 0;
            self.last_statusline_animation_tick = None;
            self.state.status = format!("Sending with {provider_id}/{model_id}");
        }
        self.state.auto_scroll = true;
        self.state.current_tip_id = None;
        self.remember_submitted_input(&message);
        self.invalidate_chat_cache();
    }

    pub(super) fn request(&mut self, req: RuntimeRequest) -> RuntimeRequestDelivery {
        if !self.runtime_bridge_connected {
            self.state.status = String::from("Runtime bridge disconnected");
            return RuntimeRequestDelivery::Disconnected;
        }

        if self.runtime_tx.send(req).is_ok() {
            return RuntimeRequestDelivery::Delivered;
        }

        self.handle_runtime_bridge_disconnect();
        RuntimeRequestDelivery::Disconnected
    }

    pub(super) fn handle_runtime_bridge_disconnect(&mut self) {
        let message = String::from("Runtime bridge disconnected");
        self.runtime_bridge_connected = false;
        self.runtime_bridge_error.get_or_insert(message.clone());
        self.state.pending_submit = None;
        self.state.optimistic_messages.clear();
        self.state.pending_script = None;
        self.stream_event_content.clear();
        self.state.is_streaming = false;
        self.state.retry_waiting = false;
        self.state.profile_locked = false;
        self.state.profile_lock_stale_after_terminal_event = false;
        self.state.script_phase = ScriptPhase::Idle;
        self.state.statusline_animation_frame = 0;
        self.last_statusline_animation_tick = None;
        self.last_stream_history_request = None;
        self.event_lag_session_resync_pending = false;
        self.event_lag_script_resync_pending = false;
        self.clear_turn_timer();
        self.invalidate_chat_cache();
        self.state.status = message.clone();
        self.state.exit = true;
        if self.is_ci_mode() && self.ci_error.is_none() {
            self.ci_turn_completion_pending = false;
            self.ci_error = Some(message);
        }
    }

    pub(super) fn invalidate_chat_cache(&mut self) {
        self.state.chat_epoch = self.state.chat_epoch.wrapping_add(1);
    }

    pub(super) fn reconcile_optimistic_messages(&mut self) {
        if self.state.optimistic_messages.is_empty() {
            return;
        }

        let before_len = self.state.optimistic_messages.len();

        let visible_chain = build_tip_chain(
            &self.state.chat_history,
            self.state.current_tip_id.as_deref(),
        );
        let mut seen_users: HashMap<String, usize> = HashMap::new();
        for msg in visible_chain {
            if msg.role() == ChatRole::User {
                let key = msg.content.text().unwrap_or_default().trim().to_string();
                *seen_users.entry(key).or_insert(0) += 1;
            }
        }

        self.state.optimistic_messages.retain(|optimistic| {
            seen_users
                .get(&optimistic.content_key)
                .is_none_or(|count| *count < optimistic.occurrence)
        });

        if self.state.optimistic_messages.len() != before_len {
            self.update_queued_status();
            self.invalidate_chat_cache();
        }
    }

    pub(super) fn visible_user_message_count(&self, content_key: &str) -> usize {
        build_tip_chain(
            &self.state.chat_history,
            self.state.current_tip_id.as_deref(),
        )
        .into_iter()
        .filter(|message| message.role() == ChatRole::User)
        .filter(|message| message.content.text().unwrap_or_default().trim() == content_key)
        .count()
    }

    pub(super) fn update_queued_status(&mut self) {
        let queued_count = self
            .state
            .optimistic_messages
            .iter()
            .filter(|message| message.is_queued)
            .count();

        if queued_count > 0 {
            self.state.status = format!("Queued message ({queued_count} queued)");
        } else if self.state.status.starts_with("Queued message (") {
            self.state.status = String::from("Queued messages sent");
        }
    }

    pub(super) fn remember_submitted_input(&mut self, message: &str) {
        let content = message.trim().to_string();
        if content.is_empty() {
            return;
        }

        self.state.input_history.insert(0, content);
        self.state.input_history.truncate(INPUT_HISTORY_LIMIT);
        self.reset_input_history_navigation();
    }
}
