use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::PathBuf;

use color_eyre::eyre::Result;
use kraai_io::fs::{ScopedReadError, read_regular_text_file, read_scoped_text_file};
use kraai_persistence::{ContextStateStore, FileContextSnapshot, InstructionSnapshot};
use kraai_types::{ContextStateEvent, ContextStateMutation, MessageId, PinnedFileScope, SessionId};

#[derive(Debug, Clone, PartialEq, Eq)]
struct PinnedFile {
    path: PathBuf,
    scope: PinnedFileScope,
    opened_event: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ContextState {
    opened_files: Vec<PinnedFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RefreshedContextState {
    pub(crate) snapshots: Vec<FileContextSnapshot>,
    pub(crate) instructions: Vec<InstructionSnapshot>,
    pub(crate) notifications: Vec<String>,
}

/// The current version of one instruction section, such as an AGENTS.md file.
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
) -> Result<RefreshedContextState> {
    let document = store.load(session_id).await?;
    let events = document.events;
    let through_event = events.last().map(|event| event.id.clone());
    let previous = document.snapshots;
    let previous_instructions = document.instructions;
    let anchor = latest.clone();
    let (mut refreshed, removals) =
        tokio::task::spawn_blocking(move || refresh_pinned_files(events, &anchor)).await?;
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
            through_event.as_deref(),
            refreshed.snapshots.clone(),
            removals,
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

/// Keeps each unchanged section where the conversation first saw it and places changed or
/// added sections after `latest`, so a change leaves the earlier conversation untouched.
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
    events: Vec<ContextStateEvent>,
    anchor: &MessageId,
) -> (RefreshedContextState, Vec<ContextStateMutation>) {
    let mut state = ContextState::default();
    for event in events {
        for mutation in event.mutations {
            state.apply(&event.id, mutation);
        }
    }
    let mut refreshed = RefreshedContextState::default();
    let mut removals = Vec::new();
    for pinned in state.opened_files {
        let text = match read_pinned_file(&pinned) {
            Ok(contents) => render_file(&pinned.path, &contents),
            Err(PinnedReadFailure::Remove(reason)) => {
                refreshed.notifications.push(format!(
                    "{} was automatically unpinned because {reason}.",
                    pinned.path.display()
                ));
                removals.push(ContextStateMutation::UnpinFile {
                    path: pinned.path,
                    reason: Some(reason),
                });
                continue;
            }
            Err(PinnedReadFailure::Unavailable(error)) => {
                format!(
                    "Opened file: {}\n[temporarily unavailable: {error}]",
                    pinned.path.display()
                )
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

fn render_file(path: &std::path::Path, contents: &str) -> String {
    let mut text = format!(
        "Opened file: {}\nFormat: <line>|<content>.\n```text\n",
        path.display()
    );
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

impl ContextState {
    fn apply(&mut self, event_id: &str, mutation: ContextStateMutation) {
        match mutation {
            ContextStateMutation::PinFile { path, scope } => {
                if let Some(existing) = self
                    .opened_files
                    .iter_mut()
                    .find(|existing| existing.path == path)
                {
                    existing.scope = scope;
                } else {
                    self.opened_files.push(PinnedFile {
                        path,
                        scope,
                        opened_event: event_id.into(),
                    });
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
    for (index, line) in contents.split_inclusive('\n').enumerate() {
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
            ("\n", "1|\n"),
            ("\n\n", "1|\n2|\n"),
            ("one\n", "1|one\n"),
            ("one\r\ntwo\n\n", "1|one\r\n2|two\n3|\n"),
            ("one\rtwo", "1|one\rtwo"),
            ("é\n模型\n🦀", "1|é\n2|模型\n3|🦀"),
        ] {
            let mut formatted = String::new();
            append_text_with_line_numbers(&mut formatted, input);
            assert_eq!(formatted, expected);
        }
    }

    #[test]
    fn numbered_contents_can_be_used_for_exact_edits_without_losing_line_endings() {
        for contents in [
            "",
            "\n",
            "alpha\r\nbeta\r\n",
            "alpha\r\nbeta\nlast",
            "α\n終",
        ] {
            let mut numbered = String::new();
            append_text_with_line_numbers(&mut numbered, contents);
            let old_text: String = numbered
                .split_inclusive('\n')
                .filter_map(|line| line.split_once('|').map(|(_, text)| text))
                .collect();
            assert_eq!(old_text, contents);
            let edited = kraai_workspace_fs::apply_exact_edits(
                std::path::Path::new("fixture"),
                contents,
                &[kraai_workspace_fs::ExactTextEdit {
                    start_line: 1,
                    end_line: contents.split_inclusive('\n').count().max(1) as u32,
                    old_text,
                    new_text: String::from("replacement\r\n"),
                }],
            );
            assert_eq!(edited.ok().as_deref(), Some("replacement\r\n"));
        }
    }

    #[test]
    fn state_folds_pin_reauthorization_and_unpin_in_order() {
        let path = PathBuf::from("/workspace/file.rs");
        let mut state = ContextState::default();
        state.apply(
            "opened",
            ContextStateMutation::PinFile {
                path: path.clone(),
                scope: PinnedFileScope::Workspace {
                    root: PathBuf::from("/workspace"),
                },
            },
        );
        state.apply(
            "opened",
            ContextStateMutation::PinFile {
                path: path.clone(),
                scope: PinnedFileScope::Host,
            },
        );
        assert_eq!(state.opened_files.len(), 1);
        assert_eq!(
            state.opened_files.first().map(|file| &file.scope),
            Some(&PinnedFileScope::Host)
        );
        state.apply(
            "opened",
            ContextStateMutation::UnpinFile { path, reason: None },
        );
        assert!(state.opened_files.is_empty());
    }
    fn message(id: &str, content: kraai_types::ConversationItem) -> kraai_types::Message {
        kraai_types::Message {
            id: MessageId::new(id),
            parent_id: None,
            content,
            status: kraai_types::MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        }
    }

    fn section(key: &'static str, text: &str) -> InstructionSection {
        InstructionSection {
            key,
            label: format!("{key} label"),
            text: text.into(),
        }
    }

    fn snapshot(key: &str, anchor: Option<&str>, text: &str) -> InstructionSnapshot {
        InstructionSnapshot {
            key: key.into(),
            label: format!("{key} label"),
            anchor: anchor.map(MessageId::new),
            text: text.into(),
        }
    }

    #[test]
    fn instructions_stay_in_place_until_they_change_and_report_every_change() {
        let user = || kraai_types::ConversationItem::User {
            content: "request".into(),
        };
        let first_turn = [message("user", user())];
        let mut notifications = Vec::new();
        let refreshed = refresh_instructions(
            &[],
            vec![section("agents", "rules")],
            &MessageId::new("user"),
            &first_turn.iter().map(|message| &message.id).collect(),
            &first_turn,
            &mut notifications,
        );
        assert_eq!(refreshed, [snapshot("agents", None, "rules")]);
        assert!(notifications.is_empty());

        let history = [
            message("user", user()),
            message(
                "reply",
                kraai_types::ConversationItem::Assistant { items: Vec::new() },
            ),
            message("next", user()),
        ];
        let retained = history.iter().map(|message| &message.id).collect();
        let previous = [
            snapshot("kept", Some("reply"), "same"),
            snapshot("compacted", Some("gone"), "same"),
            snapshot("changed", Some("user"), "old"),
            snapshot("removed", None, "old"),
        ];
        let refreshed = refresh_instructions(
            &previous,
            vec![
                section("kept", "same"),
                section("compacted", "same"),
                section("changed", "new"),
                section("added", "new"),
            ],
            &MessageId::new("next"),
            &retained,
            &history,
            &mut notifications,
        );
        assert_eq!(
            refreshed,
            [
                snapshot("kept", Some("reply"), "same"),
                snapshot("compacted", None, "same"),
                snapshot("changed", Some("next"), "new"),
                snapshot("added", Some("next"), "new"),
            ]
        );
        assert_eq!(
            notifications,
            [
                "removed label was removed; its instructions no longer apply.",
                "changed label changed; the version below replaces the earlier one.",
                "added label was added.",
            ]
        );
    }
}
