use super::chat_render::build_tip_chain;
use super::types::{OptimisticMessage, RuntimeRequest, ScriptApprovalAction, ScriptPhase};
use super::ui::copy_via_osc52;
use super::{App, INPUT_HISTORY_LIMIT, RuntimeRequestDelivery};
use super::{draft_images, feedback, types};
use crate::components::TextInput;
use color_eyre::eyre::Result;
use kraai_types::{ChatRole, MessageContent, SessionId};
use std::collections::HashMap;
use std::time::Instant;

impl App {
    pub(super) fn set_error(&mut self, error: String) {
        self.state.feedback.completion_until = None;
        self.state.feedback.clear_copy();
        self.state.errors.scroll.set(0);
        self.state.errors.last = Some(error);
        self.state.turn.cancelled = false;
    }

    pub(super) fn clear_error(&mut self) {
        self.state.errors.last = None;
        self.state.errors.open = false;
        self.state.errors.scroll.set(0);
    }

    pub(super) fn copy_text_to_clipboard(
        &mut self,
        text: &str,
        target: feedback::CopyTarget,
    ) -> Result<(), String> {
        self.state.feedback.clear_copy();
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
            self.state.feedback.show_copied(target, Instant::now());
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
        self.insert_input_text(ch.encode_utf8(&mut [0; 4]));
    }

    pub(super) fn insert_input_text(&mut self, text: &str) {
        let cursor = self
            .state
            .composer
            .cursor
            .min(self.state.composer.text.len());
        if self.state.composer.text.is_char_boundary(cursor) {
            self.replace_input_range(cursor..cursor, text);
        }
    }

    pub(super) fn backspace_input_char(&mut self) {
        self.reset_input_history_navigation();
        let cursor = self
            .state
            .composer
            .cursor
            .min(self.state.composer.text.len());
        if cursor == 0 || !self.state.composer.text.is_char_boundary(cursor) {
            return;
        }

        let prev = self
            .state
            .composer
            .text
            .char_indices()
            .take_while(|(idx, _)| *idx < cursor)
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.replace_input_range(prev..cursor, "");
    }

    pub(super) fn move_input_cursor_left(&mut self) {
        let cursor = self
            .state
            .composer
            .cursor
            .min(self.state.composer.text.len());
        let prev = self
            .state
            .composer
            .text
            .char_indices()
            .take_while(|(idx, _)| *idx < cursor)
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.state.composer.cursor = self.state.composer.images.snap(prev, false);
    }

    pub(super) fn delete_input_char(&mut self) {
        let cursor = self.state.composer.cursor;
        let next = self
            .state
            .composer
            .text
            .get(cursor..)
            .and_then(|text| text.chars().next())
            .map_or(cursor, |ch| cursor + ch.len_utf8());
        self.replace_input_range(cursor..next, "");
    }

    pub(super) fn move_input_cursor_right(&mut self) {
        let cursor = self
            .state
            .composer
            .cursor
            .min(self.state.composer.text.len());
        if cursor >= self.state.composer.text.len() {
            self.state.composer.cursor = self.state.composer.text.len();
            return;
        }

        let next = self
            .state
            .composer
            .text
            .char_indices()
            .map(|(idx, _)| idx)
            .find(|idx| *idx > cursor)
            .unwrap_or(self.state.composer.text.len());
        self.state.composer.cursor = self.state.composer.images.snap(next, true);
    }

    pub(super) fn reset_input_history_navigation(&mut self) {
        self.state.composer.history_index = None;
        self.state.composer.history_draft = None;
    }

    pub(super) fn handle_input_up(&mut self) {
        let nav = TextInput::cursor_navigation(
            &self.state.composer.text,
            self.state.composer.cursor,
            self.state.composer.width,
        );
        if nav.can_move_up {
            self.state.composer.cursor = self.state.composer.images.snap(nav.cursor_above, false);
            return;
        }

        if !self.state.composer.images.pending() {
            self.recall_older_input_history();
        }
    }

    pub(super) fn handle_input_down(&mut self) {
        let nav = TextInput::cursor_navigation(
            &self.state.composer.text,
            self.state.composer.cursor,
            self.state.composer.width,
        );
        if nav.can_move_down {
            self.state.composer.cursor = self.state.composer.images.snap(nav.cursor_below, true);
            return;
        }

        if !self.state.composer.images.pending() {
            self.recall_newer_input_history();
        }
    }

    fn recall_older_input_history(&mut self) {
        if self.state.composer.history.is_empty() {
            return;
        }

        let next_index = match self.state.composer.history_index {
            Some(index) => (index + 1).min(self.state.composer.history.len().saturating_sub(1)),
            None => {
                self.state.composer.history_draft = Some((
                    self.state.composer.text.clone(),
                    self.state.composer.images.clone(),
                ));
                0
            }
        };
        self.apply_input_history_index(next_index);
    }

    fn recall_newer_input_history(&mut self) {
        let Some(index) = self.state.composer.history_index else {
            return;
        };

        if index == 0 {
            let draft = self.state.composer.history_draft.take().unwrap_or_default();
            self.state.composer.text = draft.0;
            self.state.composer.images = draft.1;
            self.state.composer.cursor = self.state.composer.text.len();
            self.state.composer.history_index = None;
            return;
        }

        self.apply_input_history_index(index - 1);
    }

    fn apply_input_history_index(&mut self, index: usize) {
        let Some(message) = self.state.composer.history.get(index).cloned() else {
            return;
        };
        self.state.composer.images = draft_images::DraftImages::default();
        self.state.composer.text = message;
        self.state.composer.cursor = self.state.composer.text.len();
        self.state.composer.history_index = Some(index);
    }

    pub(super) fn select_previous_script_action(&mut self) {
        self.state.turn.script_approval_action = match self.state.turn.script_approval_action {
            ScriptApprovalAction::Allow => ScriptApprovalAction::Reject,
            ScriptApprovalAction::Reject => ScriptApprovalAction::Allow,
        };
    }

    pub(super) fn select_next_script_action(&mut self) {
        self.select_previous_script_action();
    }

    pub(super) fn confirm_current_script_action(&mut self) {
        let approved = matches!(
            self.state.turn.script_approval_action,
            ScriptApprovalAction::Allow
        );
        self.submit_script_decision(approved);
    }

    pub(super) fn submit_script_decision(&mut self, approved: bool) {
        if self.state.script_decision_pending() {
            return;
        }
        let Some(execution_id) = self
            .state
            .turn
            .pending_script
            .as_ref()
            .map(|script| script.execution_id.clone())
        else {
            return;
        };
        let Some(session_id) = self.state.current_session_id.clone() else {
            return;
        };

        let request = if approved {
            RuntimeRequest::ApproveScript {
                session_id: session_id.clone(),
                execution_id: execution_id.clone(),
            }
        } else {
            RuntimeRequest::DenyScript {
                session_id: session_id.clone(),
                execution_id: execution_id.clone(),
            }
        };
        if self.request(request) == RuntimeRequestDelivery::Delivered {
            self.clear_error();
            self.state
                .turn
                .submitted_script_decisions
                .entry(session_id)
                .or_default()
                .insert(execution_id);
        }
    }

    pub(super) fn enter_script_decision_phase(&mut self) {
        self.state.turn.approval_scroll.set(0);
        self.state.turn.approval_expanded = false;
        self.leave_sheet();
        self.state.turn.script_phase = ScriptPhase::AwaitingApproval;
        self.state.turn.script_approval_action = ScriptApprovalAction::Allow;
    }

    pub(super) fn request_sync(&mut self) {
        self.request(RuntimeRequest::GetMcpAuthStatuses);
        self.request(RuntimeRequest::ListModels);
        self.request(RuntimeRequest::ListSessions);
        self.request(RuntimeRequest::ListUserInputHistory {
            limit: INPUT_HISTORY_LIMIT,
        });
        if let Some(session_id) = self.state.current_session_id.clone() {
            self.request_sync_for_session(&session_id);
        } else {
            self.request(RuntimeRequest::GetAgentProfileCatalog);
        }
    }

    pub(super) fn request_sync_for_session(&mut self, session_id: &SessionId) {
        self.request(RuntimeRequest::GetSessionSnapshot {
            session_id: session_id.clone(),
        });
    }

    pub(super) fn reset_chat_session(&mut self, session_id: Option<SessionId>) {
        if self.state.current_session_id != session_id {
            self.clear_message_draft();
        }
        self.reset_session_state(session_id.clone());
        if let Some(messages) = self.state.sessions.failed_messages.remove(&session_id) {
            for message in messages.into_iter().rev() {
                self.recover_message_draft(message);
            }
        }
    }

    pub(super) fn reset_session_state(&mut self, session_id: Option<SessionId>) {
        let has_session = session_id.is_some();
        self.leave_sheet();
        self.state.current_session_id = session_id;
        self.state.models.last_session_model = None;
        self.restore_pending_session_model();
        self.state.chat.tip_id = None;
        self.clear_error();
        self.state.chat.history.clear();
        self.state.turn.approval_scroll.set(0);
        self.state.turn.approval_expanded = false;
        self.state.turn.context_usage = None;
        self.restore_session_cost();
        self.state.chat.optimistic_messages.clear();
        self.state.turn.pending_script = None;
        self.state.turn.running_scripts.clear();
        if has_session {
            self.state.agents.profiles.clear();
        }
        self.state.agents.warnings.clear();
        if has_session {
            self.state.agents.selected_profile_id = None;
        }
        self.state.turn.profile_locked = false;
        self.state.turn.profile_lock_stale_after_terminal_event = false;
        self.state.turn.script_approval_action = ScriptApprovalAction::Allow;
        self.state.turn.script_phase = ScriptPhase::Idle;
        self.state.turn.is_streaming = false;
        self.state.turn.retry_waiting = false;
        self.state.turn.profile_lock_stale_after_terminal_event = false;
        self.state.turn.timer = kraai_runtime::TurnTimer::default();
        self.state.feedback = feedback::VisualFeedback::default();
        self.last_statusline_animation_tick = None;
        self.state.chat.auto_scroll = true;
        self.state.chat.scroll = 0;
        self.state.turn.cancelled = false;
        self.state.chat.invalidate();
        self.clamp_chat_scroll();
    }

    pub(super) fn start_new_chat(&mut self) {
        self.clear_message_draft();
        self.state.sessions.pending_submit = None;
        self.state.sessions.pending_load_id = None;
        self.reset_chat_session(None);
        self.request(RuntimeRequest::GetAgentProfileCatalog);
    }

    pub(super) fn load_session(&mut self, session_id: SessionId) {
        let load_id = self.state.sessions.next_load_id;
        self.state.sessions.next_load_id = load_id.wrapping_add(1);
        if self.request(RuntimeRequest::LoadSession {
            load_id,
            session_id,
        }) == RuntimeRequestDelivery::Delivered
        {
            self.state.sessions.pending_load_id = Some(load_id);
        }
    }

    pub(super) fn dispatch_send_message(
        &mut self,
        session_id: SessionId,
        message: MessageContent,
        model_id: String,
        provider_id: String,
        options: kraai_types::ModelOptionValues,
        source: types::SubmissionSource,
    ) {
        let is_foreground = self.state.current_session_id.as_ref() == Some(&session_id);
        let message_was_draft = matches!(source, types::SubmissionSource::Composer { .. });
        let is_queued = matches!(source, types::SubmissionSource::Composer { queued: true });
        if self.request(RuntimeRequest::SendMessage {
            session_id: session_id.clone(),
            message: message.clone(),
            model_id,
            provider_id,
            options,
        }) == RuntimeRequestDelivery::Disconnected
        {
            if !message_was_draft {
                self.recover_session_message(Some(session_id), message);
            }
            return;
        }
        self.mark_session_model_active(&session_id);

        self.state.chat.optimistic_seq = self.state.chat.optimistic_seq.saturating_add(1);
        let local_id = format!("local-user-{}", self.state.chat.optimistic_seq);
        self.state
            .sessions
            .pending_messages
            .push_back(types::PendingMessage {
                session_id,
                message: message.clone(),
                local_id: is_foreground.then(|| local_id.clone()),
            });
        if !is_foreground {
            return;
        }
        if message_was_draft {
            self.clear_message_draft();
        }
        let display_text = message.display_text().into_owned();

        self.clear_error();
        let content_key = display_text.trim().to_string();
        let visible_count = self.visible_user_message_count(&content_key);
        let optimistic_same_count = self
            .state
            .chat
            .optimistic_messages
            .iter()
            .filter(|optimistic| optimistic.content_key == content_key)
            .count();

        self.state.chat.optimistic_messages.push(OptimisticMessage {
            local_id,
            content: display_text,
            content_key,
            occurrence: visible_count + optimistic_same_count + 1,
            is_queued,
        });

        if !is_queued {
            self.state.turn.script_phase = ScriptPhase::Idle;
            self.state.turn.profile_lock_stale_after_terminal_event = false;
            self.state.turn.is_streaming = true;
            self.state.breath_origin = Instant::now();
            self.last_statusline_animation_tick = None;
            self.state.turn.cancelled = false;
        }
        self.state.chat.auto_scroll = true;
        self.state.chat.tip_id = None;
        if let Some(text) = message.as_text() {
            self.remember_submitted_input(text);
        }
        self.state.chat.invalidate();
    }

    pub(super) fn request(&mut self, req: RuntimeRequest) -> RuntimeRequestDelivery {
        if !self.runtime_bridge_connected {
            return RuntimeRequestDelivery::Disconnected;
        }
        if !self.refreshes.start(&req) {
            return RuntimeRequestDelivery::Delivered;
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
        self.state.turn.submitted_script_decisions.clear();
        self.runtime_bridge_error.get_or_insert(message);
        let mut messages: std::collections::BTreeMap<Option<SessionId>, Vec<MessageContent>> =
            std::collections::BTreeMap::new();
        while let Some(pending) = self.state.sessions.pending_messages.pop_front() {
            messages
                .entry(Some(pending.session_id))
                .or_default()
                .push(pending.message);
        }
        if let Some(pending) = self.state.sessions.pending_submit.take() {
            messages
                .entry(pending.session_id)
                .or_default()
                .push(pending.message);
        }
        for (session_id, messages) in messages {
            self.recover_session_messages(session_id, messages);
        }
        self.state.sessions.pending_load_id = None;
        self.state.chat.optimistic_messages.clear();
        self.state.turn.pending_script = None;
        self.state.turn.is_streaming = false;
        self.state.turn.retry_waiting = false;
        self.state.turn.profile_locked = false;
        self.state.turn.profile_lock_stale_after_terminal_event = false;
        self.state.turn.script_phase = ScriptPhase::Idle;
        self.last_statusline_animation_tick = None;
        self.event_lag_session_resync_pending = false;
        self.event_lag_script_resync_pending = false;
        self.state.turn.profile_lock_stale_after_terminal_event = false;
        self.state.turn.timer = kraai_runtime::TurnTimer::default();
        self.state.feedback = feedback::VisualFeedback::default();
        self.state.chat.invalidate();
        self.state.exit = true;
    }

    pub(super) fn clear_running_scripts(&mut self) {
        if !self.state.turn.running_scripts.is_empty() {
            self.state.turn.running_scripts.clear();
            self.state.chat.invalidate_cards();
        }
    }

    pub(super) fn reconcile_optimistic_messages(&mut self) {
        if self.state.chat.optimistic_messages.is_empty() {
            return;
        }

        let before_len = self.state.chat.optimistic_messages.len();

        let visible_chain =
            build_tip_chain(&self.state.chat.history, self.state.chat.tip_id.as_deref());
        let mut seen_users: HashMap<String, usize> = HashMap::new();
        for msg in visible_chain {
            if msg.role() == ChatRole::User {
                let key = msg.display_text().trim().to_string();
                *seen_users.entry(key).or_insert(0) += 1;
            }
        }

        self.state.chat.optimistic_messages.retain(|optimistic| {
            seen_users
                .get(&optimistic.content_key)
                .is_none_or(|count| *count < optimistic.occurrence)
        });

        if self.state.chat.optimistic_messages.len() != before_len {
            self.state.chat.invalidate();
        }
    }

    pub(super) fn visible_user_message_count(&self, content_key: &str) -> usize {
        build_tip_chain(&self.state.chat.history, self.state.chat.tip_id.as_deref())
            .into_iter()
            .filter(|message| message.role() == ChatRole::User)
            .filter(|message| message.display_text().trim() == content_key)
            .count()
    }

    pub(super) fn remember_submitted_input(&mut self, message: &str) {
        let content = message.trim().to_string();
        if content.is_empty() {
            return;
        }

        self.state.composer.history.insert(0, content);
        self.state.composer.history.truncate(INPUT_HISTORY_LIMIT);
        self.reset_input_history_navigation();
    }
}
