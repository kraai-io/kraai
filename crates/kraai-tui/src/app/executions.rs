use super::*;

pub(super) fn result_summary(output: &str) -> (String, bool) {
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
    let successful = status == "completed" && exit.is_none_or(|code| code == "0");
    let mut summary = format!("Nushell · {status}");
    if let Some(exit) = exit {
        summary.push_str(&format!(" · exit {exit}"));
    }
    if let Some(elapsed) = attribute("elapsed_ms").and_then(|value| value.parse::<u64>().ok()) {
        summary.push_str(&format!(" · {elapsed} ms total"));
    }
    (summary, successful)
}

impl App {
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
            None if forward => 0,
            None => ids.len() - 1,
        };
        self.state.selected_execution = ids.get(next).cloned();
        self.invalidate_chat_cache();
        self.reveal_selected_execution();
    }

    pub(super) fn toggle_execution(&mut self) {
        if self.state.selected_execution.is_none() {
            self.select_execution(false);
        }
        let Some(id) = self.state.selected_execution.clone() else {
            return;
        };
        let default_expanded = self
            .state
            .chat_history
            .values()
            .find_map(|message| match &message.content {
                ConversationItem::ScriptResult { call_id, output } if call_id.as_str() == id => {
                    Some(!result_summary(output).1)
                }
                _ => None,
            })
            .unwrap_or(false);
        let expanded = self
            .state
            .execution_expanded
            .entry(id)
            .or_insert(default_expanded);
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
