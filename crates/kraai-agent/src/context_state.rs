use std::fs;
use std::path::PathBuf;

use color_eyre::eyre::Result;
use kraai_persistence::{ContextStateMutation, ContextStateStore, PinnedFileScope};
use kraai_workspace_fs::{ScopedReadError, read_scoped_text_file};

const REFRESH_COMPONENT: &str = "pinned-file-refresh";

#[derive(Debug, Clone, PartialEq, Eq)]
struct PinnedFile {
    path: PathBuf,
    scope: PinnedFileScope,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ContextState {
    opened_files: Vec<PinnedFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RefreshedContextState {
    pub(crate) prompt: String,
    pub(crate) notifications: Vec<String>,
}

pub(crate) async fn refresh_context_state(
    store: &dyn ContextStateStore,
    session_id: &str,
) -> Result<RefreshedContextState> {
    let mut state = ContextState::default();
    for event in store.list(session_id).await? {
        for mutation in event.mutations {
            state.apply(&mutation);
        }
    }

    let mut sections = Vec::new();
    let mut removals = Vec::new();
    let mut notifications = Vec::new();
    for pinned in &state.opened_files {
        match read_pinned_file(pinned) {
            Ok(contents) => sections.push(format!(
                "File: {}\n```text\n{}\n```",
                pinned.path.display(),
                format_text_with_line_numbers(&contents)
            )),
            Err(PinnedReadFailure::Remove(reason)) => {
                notifications.push(format!(
                    "{} was automatically unpinned because {reason}.",
                    pinned.path.display()
                ));
                removals.push(ContextStateMutation::UnpinFile {
                    path: pinned.path.clone(),
                    reason: Some(reason),
                });
            }
            Err(PinnedReadFailure::Unavailable(error)) => sections.push(format!(
                "File: {}\n```text\n[temporarily unavailable: {error}]\n```",
                pinned.path.display()
            )),
        }
    }

    if !removals.is_empty() {
        store
            .append_runtime(session_id, REFRESH_COMPONENT, removals)
            .await?;
    }

    let mut prompt_sections = Vec::new();
    if !notifications.is_empty() {
        prompt_sections.push(format!(
            "Pinned File Updates\n{}",
            notifications
                .iter()
                .map(|notification| format!("- {notification}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !sections.is_empty() {
        prompt_sections.push(format!(
            "Opened Files\nThese files are pinned into context for this turn. They are freshly read from disk before every turn and are not cached. Treat them as the authoritative current on-disk contents. Prefer this section over cat, sed, nl, or similar shell inspection commands for these paths.\n\nFormat: <line>|<content>.\n\n{}",
            sections.join("\n\n")
        ));
    }
    Ok(RefreshedContextState {
        prompt: prompt_sections.join("\n\n"),
        notifications,
    })
}

impl ContextState {
    fn apply(&mut self, mutation: &ContextStateMutation) {
        match mutation {
            ContextStateMutation::PinFile { path, scope } => {
                if let Some(existing) = self
                    .opened_files
                    .iter_mut()
                    .find(|existing| existing.path == *path)
                {
                    existing.scope = scope.clone();
                } else {
                    self.opened_files.push(PinnedFile {
                        path: path.clone(),
                        scope: scope.clone(),
                    });
                }
            }
            ContextStateMutation::UnpinFile { path, .. } => {
                self.opened_files.retain(|existing| existing.path != *path);
            }
        }
    }
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
                other => PinnedReadFailure::Unavailable(other.to_string()),
            })
        }
        PinnedFileScope::Host => fs::read_to_string(&pinned.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                PinnedReadFailure::Remove(String::from("it no longer exists"))
            } else {
                PinnedReadFailure::Unavailable(error.to_string())
            }
        }),
    }
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

    #[test]
    fn state_folds_pin_reauthorization_and_unpin_in_order() {
        let path = PathBuf::from("/workspace/file.rs");
        let mut state = ContextState::default();
        state.apply(&ContextStateMutation::PinFile {
            path: path.clone(),
            scope: PinnedFileScope::Workspace {
                root: PathBuf::from("/workspace"),
            },
        });
        state.apply(&ContextStateMutation::PinFile {
            path: path.clone(),
            scope: PinnedFileScope::Host,
        });
        assert_eq!(state.opened_files.len(), 1);
        assert_eq!(
            state.opened_files.first().map(|file| &file.scope),
            Some(&PinnedFileScope::Host)
        );
        state.apply(&ContextStateMutation::UnpinFile { path, reason: None });
        assert!(state.opened_files.is_empty());
    }
}
