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

    pub(super) fn set_tool_approval(&mut self, call_id: &str, approved: Option<bool>) {
        if let Some(tool) = self
            .state
            .pending_tools
            .iter_mut()
            .find(|tool| tool.call_id == call_id)
        {
            tool.approved = approved;
        }
    }

    pub(super) fn sort_pending_tools(&mut self) {
        self.state
            .pending_tools
            .sort_by_key(|tool| tool.queue_order);
    }

    pub(super) fn current_pending_tool(&self) -> Option<&PendingTool> {
        self.state
            .pending_tools
            .iter()
            .find(|tool| tool.approved.is_none())
    }

    pub(super) fn has_undecided_tools(&self) -> bool {
        self.state
            .pending_tools
            .iter()
            .any(|tool| tool.approved.is_none())
    }

    pub(super) fn select_previous_tool_action(&mut self) {
        self.state.tool_approval_action = match self.state.tool_approval_action {
            ToolApprovalAction::Allow => ToolApprovalAction::Reject,
            ToolApprovalAction::Reject => ToolApprovalAction::Allow,
        };
    }

    pub(super) fn select_next_tool_action(&mut self) {
        self.select_previous_tool_action();
    }

    pub(super) fn confirm_current_tool_action(&mut self) {
        let approved = matches!(self.state.tool_approval_action, ToolApprovalAction::Allow);
        self.submit_tool_decision(approved);
    }

    pub(super) fn submit_tool_decision(&mut self, approved: bool) {
        let Some(tool) = self.current_pending_tool() else {
            return;
        };
        let Some(session_id) = &self.state.current_session_id else {
            return;
        };

        if approved {
            self.request(RuntimeRequest::ApproveTool {
                session_id: session_id.clone(),
                call_id: tool.call_id.clone(),
            });
        } else {
            self.request(RuntimeRequest::DenyTool {
                session_id: session_id.clone(),
                call_id: tool.call_id.clone(),
            });
        }
    }

    pub(super) fn enter_tool_decision_phase(&mut self) {
        self.state.mode = UiMode::Chat;
        self.state.tool_phase = ToolPhase::Deciding;
        self.state.tool_approval_action = ToolApprovalAction::Allow;
    }

    pub(super) fn sync_tool_phase_from_pending_tools(&mut self) {
        self.sort_pending_tools();
        if self.has_undecided_tools() {
            self.enter_tool_decision_phase();
            return;
        }

        if !self.state.pending_tools.is_empty() {
            self.state.mode = UiMode::Chat;
            self.state.tool_phase = ToolPhase::ExecutingBatch;
        } else if self.state.tool_phase != ToolPhase::ExecutingBatch {
            self.state.tool_phase = ToolPhase::Idle;
            self.state.tool_batch_execution_started = false;
        }
    }

    pub(super) fn maybe_start_tool_batch_execution(&mut self) {
        if self.state.tool_phase != ToolPhase::ExecutingBatch
            || self.state.tool_batch_execution_started
            || self.state.pending_tools.is_empty()
            || self.has_undecided_tools()
        {
            return;
        }

        let Some(session_id) = &self.state.current_session_id else {
            return;
        };

        self.state.tool_batch_execution_started = true;
        self.state.status = format!(
            "Executing {} decided tool call(s)",
            self.state.pending_tools.len()
        );
        self.request(RuntimeRequest::ExecuteApprovedTools {
            session_id: session_id.clone(),
        });
    }

    pub(super) fn finish_tool_batch_execution(&mut self) {
        self.state.tool_phase = ToolPhase::Idle;
        self.state.tool_batch_execution_started = false;
    }

    pub(super) fn request_sync(&self) {
        self.request(RuntimeRequest::ListModels);
        self.request(RuntimeRequest::ListSessions);
        self.request(RuntimeRequest::ListUserInputHistory {
            limit: INPUT_HISTORY_LIMIT,
        });
        if let Some(session_id) = &self.state.current_session_id {
            self.request_sync_for_session(session_id);
        }
    }

    pub(super) fn request_sync_for_session(&self, session_id: &str) {
        self.request(RuntimeRequest::GetCurrentTip {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetChatHistory {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetSessionContextUsage {
            session_id: session_id.to_string(),
        });
        self.request(RuntimeRequest::GetPendingTools {
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
        self.state.optimistic_tool_messages.clear();
        self.stream_event_content.clear();
        self.state.pending_tools.clear();
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
        self.state.tool_approval_action = ToolApprovalAction::Allow;
        self.state.tool_phase = ToolPhase::Idle;
        self.state.tool_batch_execution_started = false;
        self.state.is_streaming = false;
        self.state.retry_waiting = false;
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
            self.state.is_streaming = true;
            self.state.statusline_animation_frame = 0;
            self.last_statusline_animation_tick = None;
            self.state.status = format!("Sending with {provider_id}/{model_id}");
        }
        self.state.auto_scroll = true;
        self.state.current_tip_id = None;
        self.remember_submitted_input(&message);
        self.invalidate_chat_cache();

        self.request(RuntimeRequest::SendMessage {
            session_id,
            message,
            model_id,
            provider_id,
            auto_approve: self.startup_options.auto_approve,
        });
    }

    pub(super) fn request(&self, req: RuntimeRequest) {
        let _ = self.runtime_tx.send(req);
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
            if msg.role == ChatRole::User {
                let key = msg.content.trim().to_string();
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
        .filter(|message| message.role == ChatRole::User)
        .filter(|message| message.content.trim() == content_key)
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

    pub(super) fn reconcile_optimistic_tool_messages(&mut self) {
        if self.state.optimistic_tool_messages.is_empty() {
            return;
        }

        let before_len = self.state.optimistic_tool_messages.len();
        let visible_chain = build_tip_chain(
            &self.state.chat_history,
            self.state.current_tip_id.as_deref(),
        );
        let mut seen_tool_messages: HashMap<String, usize> = HashMap::new();
        for msg in visible_chain {
            if msg.role == ChatRole::Tool {
                *seen_tool_messages.entry(msg.content.clone()).or_insert(0) += 1;
            }
        }

        self.state.optimistic_tool_messages.retain(|optimistic| {
            match seen_tool_messages.get_mut(&optimistic.content) {
                Some(count) if *count > 0 => {
                    *count -= 1;
                    false
                }
                _ => true,
            }
        });

        if self.state.optimistic_tool_messages.len() != before_len {
            self.invalidate_chat_cache();
        }
    }

    pub(super) fn push_optimistic_tool_message(
        &mut self,
        call_id: &str,
        tool_id: &str,
        output: &str,
        denied: bool,
    ) {
        if self
            .state
            .optimistic_tool_messages
            .iter()
            .any(|msg| msg.local_id == format!("local-tool-{call_id}"))
        {
            return;
        }

        let output_json = serde_json::from_str(output).unwrap_or_else(|_| {
            serde_json::json!({
                "error": "Failed to parse tool result",
                "raw_output": output,
            })
        });
        let content = kraai_types::format_tool_result_message(
            &kraai_types::ToolId::new(tool_id),
            &output_json,
            denied,
        );

        self.state
            .optimistic_tool_messages
            .push(OptimisticToolMessage {
                local_id: format!("local-tool-{call_id}"),
                content,
            });
        self.invalidate_chat_cache();
    }
}
