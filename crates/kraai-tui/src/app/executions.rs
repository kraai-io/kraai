use super::*;

pub(super) fn result_summary(output: &str) -> String {
    let header = output
        .strip_prefix("<tool_call_result ")
        .and_then(|text| text.split_once('>').map(|(header, _)| header));
    let attribute = |name: &str| {
        header.and_then(|header| {
            header.split_whitespace().find_map(|part| {
                part.strip_prefix(name)
                    .and_then(|value| value.strip_prefix("=\""))
                    .and_then(|value| value.strip_suffix('"'))
            })
        })
    };
    let status = attribute("status").unwrap_or("unknown");
    let exit = attribute("exit_code");
    let mut summary = if status == "completed" {
        format!("exit {}", exit.unwrap_or("unknown"))
    } else if status == "cancelled" {
        String::from("cancelled")
    } else {
        match exit {
            Some(exit) => format!("{status} · exit {exit}"),
            None => status.to_string(),
        }
    };
    if let Some(elapsed) = attribute("elapsed_ms").and_then(|value| value.parse::<u64>().ok()) {
        summary.push_str(&format!(
            " · {}",
            super::duration::format_duration(Duration::from_millis(elapsed))
        ));
    }
    summary
}

impl App {
    pub(super) fn open_executions(&mut self) {
        self.state.mode = UiMode::Executions;
        self.state.selected_execution = None;
        self.state.ctrl_c_exit_armed = false;
        self.invalidate_chat_cache();
        self.select_execution(false);
    }

    pub(super) fn close_executions(&mut self) {
        self.state.execution_expanded.clear();
        self.state.selected_execution = None;
        self.state.mode = UiMode::Chat;
        self.state.auto_scroll = true;
        self.invalidate_chat_cache();
    }

    pub(super) fn handle_execution_key_event(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::F(6) | KeyCode::Char('q') => self.close_executions(),
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => self.close_executions(),
            KeyCode::Up => self.select_execution(false),
            KeyCode::Down => self.select_execution(true),
            KeyCode::Enter => self.toggle_execution(),
            KeyCode::PageUp => self.scroll_chat_by(-10),
            KeyCode::PageDown => self.scroll_chat_by(10),
            KeyCode::Home => self.scroll_chat_to_top(),
            KeyCode::End => self.scroll_chat_to_bottom(),
            _ => {}
        }
    }

    pub(super) fn select_execution(&mut self, forward: bool) {
        let ids: Vec<String> = self
            .state
            .rendered_messages()
            .iter()
            .filter_map(|message| match &message.content {
                ConversationItem::ScriptResult { call_id, .. } => Some(call_id.to_string()),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            if self.state.selected_execution.take().is_some() {
                self.invalidate_chat_cache();
            }
            return;
        }
        let index = self
            .state
            .selected_execution
            .as_ref()
            .and_then(|selected| ids.iter().position(|id| id == selected));
        let next = match index {
            Some(index) if forward => (index + 1) % ids.len(),
            Some(index) => (index + ids.len() - 1) % ids.len(),
            None => ids.len() - 1,
        };
        self.state.selected_execution = ids.get(next).cloned();
        self.invalidate_chat_cache();
        self.reveal_selected_execution();
    }

    pub(super) fn toggle_execution(&mut self) {
        let selection_visible = self.state.selected_execution.as_ref().is_some_and(|selected| {
            self.state.rendered_messages().iter().any(|message| {
                matches!(&message.content, ConversationItem::ScriptResult { call_id, .. } if call_id.as_str() == selected)
            })
        });
        if !selection_visible {
            self.select_execution(false);
        }
        let Some(id) = self.state.selected_execution.clone() else {
            return;
        };
        let expanded = self.state.execution_expanded.entry(id).or_insert(false);
        *expanded = !*expanded;
        self.invalidate_chat_cache();
        self.reveal_selected_execution();
    }

    fn reveal_selected_execution(&mut self) {
        let width = self.state.chat_render_cache.borrow().width;
        self.state.refresh_chat_render_cache(width);
        let offset = self.state.selected_execution.as_ref().and_then(|id| {
            self.state
                .chat_render_cache
                .borrow()
                .execution_offsets
                .get(id)
                .copied()
        });
        if let Some(offset) = offset {
            self.state.auto_scroll = false;
            self.state.scroll = offset.min(self.state.chat_max_scroll());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::result_summary;

    #[test]
    fn summaries_show_exit_or_cancellation_without_redundant_labels() {
        for (status, expected) in [
            ("completed", "exit 1 · 0.5s"),
            ("cancelled", "cancelled · 0.5s"),
            ("timed-out", "timed-out · exit 1 · 0.5s"),
        ] {
            assert_eq!(
                result_summary(&format!(
                    "<tool_call_result status=\"{status}\" exit_code=\"1\" elapsed_ms=\"500\"></tool_call_result>"
                )),
                expected
            );
        }
    }
}
