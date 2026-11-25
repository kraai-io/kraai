use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use color_eyre::eyre::Result;
use kraai_io::fs::{ScopedReadError, read_regular_text_file, read_scoped_text_file};
use kraai_persistence::{ContextStateStore, FileContextSnapshot, InstructionSnapshot, PinnedFile};
use kraai_types::{ContextStateMutation, MessageId, PinnedFileScope, SessionId};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RefreshedContextState {
    pub(crate) snapshots: Vec<FileContextSnapshot>,
    pub(crate) instructions: Vec<InstructionSnapshot>,
    pub(crate) notifications: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstructionSection {
    pub(crate) key: &'static str,
    pub(crate) label: String,
    pub(crate) text: String,
}

pub(crate) fn append_notifications(
    request: &mut kraai_provider_core::ProviderRequest,
    notifications: &[String],
) {
    if !notifications.is_empty() {
        request
            .messages
            .push(kraai_types::ConversationItem::FileContext {
                text: notifications.join("\n"),
            });
    }
}

pub(crate) async fn refresh_context_state(
    store: &dyn ContextStateStore,
    session_id: &SessionId,
    latest: &MessageId,
    history: &[kraai_types::Message],
    compacted: Option<&MessageId>,
    instructions: Vec<InstructionSection>,
    workspace: &Path,
) -> Result<RefreshedContextState> {
    let document = store.load(session_id).await?;
    let pinned = document.pinned_files();
    let revision = document.revision;
    let previous = document.snapshots;
    let previous_instructions = document.instructions;
    let anchor = latest.clone();
    let workspace = workspace.to_path_buf();
    let (mut refreshed, removals) =
        tokio::task::spawn_blocking(move || refresh_pinned_files(pinned, &anchor, &workspace))
            .await?;
    let previous: HashMap<_, _> = previous
        .iter()
        .enumerate()
        .map(|(position, snapshot)| (&snapshot.path, (position, snapshot)))
        .collect();
    let unchanged = |snapshot: &FileContextSnapshot| {
        previous.get(&snapshot.path).filter(|(_, old)| {
            old.opened_event == snapshot.opened_event && old.text == snapshot.text
        })
    };
    let retained: HashSet<_> = history.iter().map(|message| &message.id).collect();
    for snapshot in &mut refreshed.snapshots {
        snapshot.anchor = match unchanged(snapshot) {
            Some((_, old)) if retained.contains(&old.anchor) => old.anchor.clone(),
            Some(_) => compacted.unwrap_or(latest).clone(),
            _ => latest.clone(),
        };
    }
    refreshed
        .snapshots
        .sort_by_key(|snapshot| unchanged(snapshot).map_or(usize::MAX, |(position, _)| *position));
    store
        .save_snapshots(
            session_id,
            revision.as_deref(),
            refreshed.snapshots.clone(),
            removals,
            &history.iter().map(|message| message.id.clone()).collect(),
        )
        .await?;
    refreshed.instructions = refresh_instructions(
        &previous_instructions,
        instructions,
        latest,
        &retained,
        history,
        &mut refreshed.notifications,
    );
    if refreshed.instructions != previous_instructions {
        store
            .save_instructions(session_id, refreshed.instructions.clone())
            .await?;
    }
    Ok(refreshed)
}

/// Changed or added sections go after `latest` so the earlier conversation stays untouched.
fn refresh_instructions(
    previous: &[InstructionSnapshot],
    current: Vec<InstructionSection>,
    latest: &MessageId,
    retained: &HashSet<&MessageId>,
    history: &[kraai_types::Message],
    notifications: &mut Vec<String>,
) -> Vec<InstructionSnapshot> {
    let first_request = !history.iter().any(|message| {
        matches!(
            message.content,
            kraai_types::ConversationItem::Assistant { .. }
        )
    });
    for removed in previous
        .iter()
        .filter(|old| !current.iter().any(|section| section.key == old.key))
    {
        notifications.push(format!(
            "{} was removed; its instructions no longer apply.",
            removed.label
        ));
    }
    current
        .into_iter()
        .map(|section| {
            let old = previous.iter().find(|old| old.key == section.key);
            let anchor = match old {
                Some(old) if old.text == section.text => old
                    .anchor
                    .clone()
                    .filter(|anchor| retained.contains(anchor)),
                Some(_) => {
                    notifications.push(format!(
                        "{} changed; the version below replaces the earlier one.",
                        section.label
                    ));
                    Some(latest.clone())
                }
                None if first_request => None,
                None => {
                    notifications.push(format!("{} was added.", section.label));
                    Some(latest.clone())
                }
            };
            InstructionSnapshot {
                key: section.key.into(),
                label: section.label,
                anchor,
                text: section.text,
            }
        })
        .collect()
}

fn refresh_pinned_files(
    pinned_files: Vec<PinnedFile>,
    anchor: &MessageId,
    workspace: &Path,
) -> (RefreshedContextState, Vec<ContextStateMutation>) {
    let mut refreshed = RefreshedContextState::default();
    let mut removals = Vec::new();
    for pinned in pinned_files {
        let shown = display_path(&pinned.path, workspace);
        let text = match read_pinned_file(&pinned) {
            Ok(contents) => render_file(&shown, &contents),
            Err(PinnedReadFailure::Remove(reason)) => {
                refreshed.notifications.push(format!(
                    "{shown} was automatically unpinned because {reason}."
                ));
                removals.push(ContextStateMutation::UnpinFile {
                    path: pinned.path,
                    reason: Some(reason),
                });
                continue;
            }
            Err(PinnedReadFailure::Unavailable(error)) => {
                format!("Opened file: {shown}\n[temporarily unavailable: {error}]")
            }
        };
        refreshed.snapshots.push(FileContextSnapshot {
            path: pinned.path,
            opened_event: pinned.opened_event,
            anchor: anchor.clone(),
            text,
        });
    }
    (refreshed, removals)
}

fn display_path(path: &Path, workspace: &Path) -> String {
    let path = without_verbatim_prefix(path);
    match path.strip_prefix(without_verbatim_prefix(workspace)) {
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

fn without_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

fn render_file(path: &str, contents: &str) -> String {
    let mut text = format!("Opened file: {path}\n```text\n");
    append_text_with_line_numbers(&mut text, contents);
    if !contents.is_empty() && !contents.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("```");
    if !contents.is_empty() && !contents.ends_with('\n') {
        text.push_str("\nNo newline at end of file.");
    }
    text
}

enum PinnedReadFailure {
    Remove(String),
    Unavailable(String),
}

fn read_pinned_file(pinned: &PinnedFile) -> Result<String, PinnedReadFailure> {
    match &pinned.scope {
        PinnedFileScope::Workspace { root } => {
            read_scoped_text_file(root, &pinned.path).map_err(|error| match error {
                ScopedReadError::NotFound(_) => {
                    PinnedReadFailure::Remove(String::from("it no longer exists"))
                }
                ScopedReadError::OutsideRoot(_) => PinnedReadFailure::Remove(String::from(
                    "it no longer resolves within its authorized workspace",
                )),
                ScopedReadError::NotFile(_) => {
                    PinnedReadFailure::Remove(String::from("it is no longer a regular file"))
                }
                ScopedReadError::OpenRoot { source, .. }
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    PinnedReadFailure::Remove(String::from(
                        "its authorized workspace no longer exists",
                    ))
                }
                other @ (ScopedReadError::OpenRoot { .. }
                | ScopedReadError::Open { .. }
                | ScopedReadError::Inspect { .. }
                | ScopedReadError::ReadText { .. }
                | ScopedReadError::UnsupportedPlatform(_)) => {
                    PinnedReadFailure::Unavailable(other.to_string())
                }
            })
        }
        PinnedFileScope::Host => read_regular_text_file(&pinned.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                PinnedReadFailure::Remove(String::from("it no longer exists"))
            } else {
                PinnedReadFailure::Unavailable(error.to_string())
            }
        }),
    }
}

fn append_text_with_line_numbers(formatted: &mut String, contents: &str) {
    formatted.reserve(contents.len());
    for (index, line) in contents.split_inclusive('\n').enumerate() {
        let _ = write!(formatted, "{}|{line}", index.saturating_add(1));
    }
}

#[cfg(test)]
mod tests;
