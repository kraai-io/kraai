use std::fmt::Write;
use std::path::PathBuf;

use color_eyre::eyre::Result;
use kraai_persistence::ContextStateStore;
use kraai_types::{ContextStateEvent, ContextStateMutation, PinnedFileScope};
use kraai_workspace_fs::{ScopedReadError, read_regular_text_file, read_scoped_text_file};

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
    let events = store.list(session_id).await?;
    if events.is_empty() {
        return Ok(RefreshedContextState::default());
    }
    let (refreshed, removals) =
        tokio::task::spawn_blocking(move || refresh_pinned_files(events)).await?;
    if !removals.is_empty() {
        store
            .append_runtime(session_id, REFRESH_COMPONENT, removals)
            .await?;
    }
    Ok(refreshed)
}

fn refresh_pinned_files(
    events: Vec<ContextStateEvent>,
) -> (RefreshedContextState, Vec<ContextStateMutation>) {
    let mut state = ContextState::default();
    for event in events {
        for mutation in event.mutations {
            state.apply(mutation);
        }
    }

    let mut sections = String::new();
    let mut removals = Vec::new();
    let mut notifications = Vec::new();
    for pinned in state.opened_files {
        match read_pinned_file(&pinned) {
            Ok(contents) => {
                let mut numbered = String::with_capacity(contents.len());
                append_text_with_line_numbers(&mut numbered, &contents);
                begin_file_section(&mut sections, &pinned, Some(numbered.len().div_ceil(4)));
                sections.push_str(&numbered);
                sections.push_str("\n```");
            }
            Err(PinnedReadFailure::Remove(reason)) => {
                notifications.push(format!(
                    "{} was automatically unpinned because {reason}.",
                    pinned.path.display()
                ));
                removals.push(ContextStateMutation::UnpinFile {
                    path: pinned.path,
                    reason: Some(reason),
                });
            }
            Err(PinnedReadFailure::Unavailable(error)) => {
                begin_file_section(&mut sections, &pinned, None);
                let _ = write!(sections, "[temporarily unavailable: {error}]\n```");
            }
        }
    }

    let prompt = if notifications.is_empty() {
        sections
    } else {
        let mut prompt = String::from("Pinned File Updates");
        for notification in &notifications {
            let _ = write!(prompt, "\n- {notification}");
        }
        if !sections.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&sections);
        }
        prompt
    };
    (
        RefreshedContextState {
            prompt,
            notifications,
        },
        removals,
    )
}

fn begin_file_section(
    sections: &mut String,
    pinned: &PinnedFile,
    approximate_tokens: Option<usize>,
) {
    if sections.is_empty() {
        sections.push_str("Opened Files\nThese files are freshly read from disk and included in every model request until closed. Treat them as the authoritative current on-disk contents. Prefer this section over repeated shell reads of these paths. Close files with kraai-close-files after extracting needed information, finishing an edit, or moving to other work, unless you still need their contents. You can reopen them later. Context estimates below use roughly one token per four UTF-8 bytes of line-numbered text; actual token counts and cache hits depend on the model.\n\nFormat: <line>|<content>.\n\n");
    } else {
        sections.push_str("\n\n");
    }
    let _ = writeln!(sections, "File: {}", pinned.path.display());
    if let Some(tokens) = approximate_tokens {
        let _ = writeln!(
            sections,
            "Approximate content context per request: {tokens} tokens"
        );
    }
    sections.push_str("```text\n");
}

impl ContextState {
    fn apply(&mut self, mutation: ContextStateMutation) {
        match mutation {
            ContextStateMutation::PinFile { path, scope } => {
                if let Some(existing) = self
                    .opened_files
                    .iter_mut()
                    .find(|existing| existing.path == path)
                {
                    existing.scope = scope;
                } else {
                    self.opened_files.push(PinnedFile { path, scope });
                }
            }
            ContextStateMutation::UnpinFile { path, .. } => {
                self.opened_files.retain(|existing| existing.path != path);
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
    for (index, line) in contents.lines().enumerate() {
        if index > 0 {
            formatted.push('\n');
        }
        let _ = write!(formatted, "{}|{line}", index.saturating_add(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_numbering_preserves_blank_lines_line_endings_and_unicode() {
        for (input, expected) in [
            ("", ""),
            ("\n", "1|"),
            ("\n\n", "1|\n2|"),
            ("one\n", "1|one"),
            ("one\r\ntwo\n\n", "1|one\n2|two\n3|"),
            ("one\rtwo", "1|one\rtwo"),
            ("é\n模型\n🦀", "1|é\n2|模型\n3|🦀"),
        ] {
            let mut formatted = String::new();
            append_text_with_line_numbers(&mut formatted, input);
            assert_eq!(formatted, expected);
        }
    }

    #[test]
    fn state_folds_pin_reauthorization_and_unpin_in_order() {
        let path = PathBuf::from("/workspace/file.rs");
        let mut state = ContextState::default();
        state.apply(ContextStateMutation::PinFile {
            path: path.clone(),
            scope: PinnedFileScope::Workspace {
                root: PathBuf::from("/workspace"),
            },
        });
        state.apply(ContextStateMutation::PinFile {
            path: path.clone(),
            scope: PinnedFileScope::Host,
        });
        assert_eq!(state.opened_files.len(), 1);
        assert_eq!(
            state.opened_files.first().map(|file| &file.scope),
            Some(&PinnedFileScope::Host)
        );
        state.apply(ContextStateMutation::UnpinFile { path, reason: None });
        assert!(state.opened_files.is_empty());
    }
}
