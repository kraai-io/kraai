use std::path::Path;

use color_eyre::eyre::Result;
use kraai_persistence::ScriptExecutionStore;
use kraai_types::ContextStateDelta;

const OPENED_FILES_NAMESPACE: &str = "opened_files";
const OPEN_OPERATION: &str = "open";
const CLOSE_OPERATION: &str = "close";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContextState {
    opened_files: Vec<String>,
}

pub(crate) async fn resolve_context_state(
    store: &dyn ScriptExecutionStore,
    session_id: &str,
) -> Result<ContextState> {
    let records = store.list_for_session(session_id).await?;
    let mut state = ContextState::default();
    for record in records {
        for effect in record.acknowledged_effects {
            for delta in effect.deltas {
                state.apply(&delta);
            }
        }
    }
    Ok(state)
}

impl ContextState {
    fn apply(&mut self, delta: &ContextStateDelta) {
        if delta.namespace != OPENED_FILES_NAMESPACE {
            return;
        }
        let Some(path) = delta
            .payload
            .get("path")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        match delta.operation.as_str() {
            OPEN_OPERATION => {
                if !self.opened_files.iter().any(|existing| existing == path) {
                    self.opened_files.push(path.to_string());
                }
            }
            CLOSE_OPERATION => self.opened_files.retain(|existing| existing != path),
            _ => {}
        }
    }
}

pub(crate) fn render_context_state(state: &ContextState, _workspace_dir: &Path) -> String {
    if state.opened_files.is_empty() {
        return String::new();
    }
    let mut sections = vec![String::from(
        "Opened Files\nThese files are pinned into context for this turn. They are freshly read from disk before every turn and are not cached. Treat them as the authoritative current on-disk contents. Prefer this section over cat, sed, nl, or similar shell inspection commands for these paths.\n\nFormat: <line>|<content>.",
    )];
    for path in &state.opened_files {
        let rendered = std::fs::read_to_string(path).map_or_else(
            |error| format!("[unavailable: {error}]"),
            |contents| format_text_with_line_numbers(&contents),
        );
        sections.push(format!("File: {path}\n```text\n{rendered}\n```"));
    }
    sections.join("\n\n")
}

fn format_text_with_line_numbers(contents: &str) -> String {
    contents
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{}|{line}", index.saturating_add(1)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn delta(operation: &str, path: &str) -> ContextStateDelta {
        ContextStateDelta {
            namespace: String::from(OPENED_FILES_NAMESPACE),
            operation: operation.to_string(),
            payload: json!({ "path": path }),
        }
    }

    #[test]
    fn acknowledged_open_and_close_effects_fold_in_order() {
        let mut state = ContextState::default();
        state.apply(&delta(OPEN_OPERATION, "/tmp/a"));
        state.apply(&delta(OPEN_OPERATION, "/tmp/b"));
        state.apply(&delta(CLOSE_OPERATION, "/tmp/a"));
        assert_eq!(state.opened_files, vec![String::from("/tmp/b")]);
    }
}
