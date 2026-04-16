use std::{collections::BTreeMap, path::Path};

use kraai_tool_core::{
    FILE_READS_NAMESPACE, FILE_READS_OPERATION_REFRESH, format_text_with_line_numbers,
    read_text_path,
};
use kraai_types::{Message, ToolStateDelta, ToolStateSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const OPENED_FILES_NAMESPACE: &str = "opened_files";
const OPENED_FILES_OPERATION_OPEN: &str = "open";
const OPENED_FILES_OPERATION_CLOSE: &str = "close";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct OpenedFilesState {
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct FileReadsState {
    #[serde(default)]
    by_path: BTreeMap<String, String>,
}

pub fn resolve_snapshot_from_history(history: &[Message]) -> ToolStateSnapshot {
    let mut snapshot = history
        .iter()
        .rev()
        .find_map(|message| message.tool_state_snapshot.clone())
        .unwrap_or_default();

    let start_index = history
        .iter()
        .rposition(|message| message.tool_state_snapshot.is_some())
        .map(|index| index + 1)
        .unwrap_or(0);

    for message in &history[start_index..] {
        apply_deltas(&mut snapshot, &message.tool_state_deltas);
    }

    snapshot
}

pub fn refresh_and_render_system_prompt(
    snapshot: &mut ToolStateSnapshot,
    _workspace_dir: &Path,
) -> String {
    let opened_files = opened_files_from_snapshot(snapshot);
    if opened_files.paths.is_empty() {
        return String::new();
    }

    let mut file_reads = file_reads_from_snapshot(snapshot);
    let mut sections = vec![String::from(
        "Opened Files\nThese files are pinned into context for this turn. Their contents below are the current on-disk versions. Do not call read_files for these paths unless you need a separate explicit read result.",
    )];

    for path in opened_files.paths {
        let rendered_contents = match read_text_path(Path::new(&path)) {
            Ok(read) => {
                file_reads
                    .by_path
                    .insert(path.clone(), read.sha256().to_string());
                format_text_with_line_numbers(read.contents())
            }
            Err(error) => {
                file_reads.by_path.remove(&path);
                format!("[unavailable: {error}]")
            }
        };
        sections.push(format!("File: {path}\n```text\n{rendered_contents}\n```"));
    }

    write_file_reads_to_snapshot(snapshot, file_reads);
    sections.join("\n\n")
}

fn opened_files_from_snapshot(snapshot: &ToolStateSnapshot) -> OpenedFilesState {
    snapshot
        .entries
        .get(OPENED_FILES_NAMESPACE)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn file_reads_from_snapshot(snapshot: &ToolStateSnapshot) -> FileReadsState {
    snapshot
        .entries
        .get(FILE_READS_NAMESPACE)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn write_file_reads_to_snapshot(snapshot: &mut ToolStateSnapshot, state: FileReadsState) {
    if state.by_path.is_empty() {
        snapshot.entries.remove(FILE_READS_NAMESPACE);
    } else {
        snapshot
            .entries
            .insert(String::from(FILE_READS_NAMESPACE), json!(state));
    }
}

#[cfg(test)]
fn open_file_delta(path: String) -> ToolStateDelta {
    ToolStateDelta {
        namespace: String::from(OPENED_FILES_NAMESPACE),
        operation: String::from(OPENED_FILES_OPERATION_OPEN),
        payload: json!({ "path": path }),
    }
}

#[cfg(test)]
fn close_file_delta(path: String) -> ToolStateDelta {
    ToolStateDelta {
        namespace: String::from(OPENED_FILES_NAMESPACE),
        operation: String::from(OPENED_FILES_OPERATION_CLOSE),
        payload: json!({ "path": path }),
    }
}

fn apply_deltas(snapshot: &mut ToolStateSnapshot, deltas: &[ToolStateDelta]) {
    for delta in deltas {
        match delta.namespace.as_str() {
            OPENED_FILES_NAMESPACE => apply_opened_file_delta(snapshot, delta),
            FILE_READS_NAMESPACE => apply_file_read_delta(snapshot, delta),
            _ => {}
        }
    }
}

fn apply_opened_file_delta(snapshot: &mut ToolStateSnapshot, delta: &ToolStateDelta) {
    let path = delta
        .payload
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let Some(path) = path else {
        return;
    };

    let mut state = opened_files_from_snapshot(snapshot);
    match delta.operation.as_str() {
        OPENED_FILES_OPERATION_OPEN => {
            if !state.paths.iter().any(|existing| existing == &path) {
                state.paths.push(path);
            }
        }
        OPENED_FILES_OPERATION_CLOSE => {
            state.paths.retain(|existing| existing != &path);
        }
        _ => return,
    }

    if state.paths.is_empty() {
        snapshot.entries.remove(OPENED_FILES_NAMESPACE);
    } else {
        snapshot
            .entries
            .insert(String::from(OPENED_FILES_NAMESPACE), json!(state));
    }
}

fn apply_file_read_delta(snapshot: &mut ToolStateSnapshot, delta: &ToolStateDelta) {
    if delta.operation != FILE_READS_OPERATION_REFRESH {
        return;
    }

    let path = delta
        .payload
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let sha256 = delta
        .payload
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    let (Some(path), Some(sha256)) = (path, sha256) else {
        return;
    };

    let mut state = file_reads_from_snapshot(snapshot);
    state.by_path.insert(path, sha256);
    write_file_reads_to_snapshot(snapshot, state);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use kraai_tool_core::file_read_sha256;
    use kraai_types::{ChatRole, Message, MessageId, MessageStatus};
    use serde_json::json;

    use super::{
        close_file_delta, open_file_delta, opened_files_from_snapshot,
        refresh_and_render_system_prompt, resolve_snapshot_from_history,
    };

    fn message(id: &str) -> Message {
        Message {
            id: MessageId::new(id),
            parent_id: None,
            role: ChatRole::User,
            content: String::new(),
            status: MessageStatus::Complete,
            agent_profile_id: None,
            tool_state_snapshot: None,
            tool_state_deltas: Vec::new(),
        }
    }

    #[test]
    fn resolves_snapshot_from_latest_ancestor_and_replays_newer_deltas() {
        let mut snapshot_message = message("snapshot");
        snapshot_message.tool_state_snapshot = Some(kraai_types::ToolStateSnapshot {
            entries: BTreeMap::from([(
                String::from("opened_files"),
                json!({ "paths": ["/tmp/a.rs"] }),
            )]),
        });

        let mut open_message = message("open");
        open_message.tool_state_deltas = vec![open_file_delta(String::from("/tmp/b.rs"))];

        let mut close_message = message("close");
        close_message.tool_state_deltas = vec![close_file_delta(String::from("/tmp/a.rs"))];

        let snapshot =
            resolve_snapshot_from_history(&[snapshot_message, open_message, close_message]);
        let opened = opened_files_from_snapshot(&snapshot);

        assert_eq!(opened.paths, vec![String::from("/tmp/b.rs")]);
    }

    #[test]
    fn refresh_opened_files_updates_file_read_hashes() {
        let path = std::env::temp_dir().join(format!("tool-state-refresh-{}", std::process::id()));
        std::fs::write(&path, "alpha\nbeta\n").expect("write temp file");

        let mut snapshot = kraai_types::ToolStateSnapshot {
            entries: BTreeMap::from([(
                String::from("opened_files"),
                json!({ "paths": [path.display().to_string()] }),
            )]),
        };

        let prompt = refresh_and_render_system_prompt(&mut snapshot, Path::new("/tmp"));
        assert!(prompt.contains("1|alpha"));
        assert!(file_read_sha256(&snapshot, &path).is_some());

        let _ = std::fs::remove_file(path);
    }
}
