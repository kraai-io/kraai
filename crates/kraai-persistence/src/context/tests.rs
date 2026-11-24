use super::*;
use tokio::fs;

#[tokio::test]
async fn unpin_commits_remove_only_closed_snapshots_without_a_refresh() {
    for source in ["command", "runtime", "refresh"] {
        let root = std::env::temp_dir().join(format!("file-context-close-{}", Ulid::generate()));
        fs::create_dir(&root).await.unwrap();
        let store = SqliteContextStateStore::new(&root);
        let closed = root.join("closed.txt");
        let retained = root.join("retained.txt");
        let event = store
            .append_runtime(
                &SessionId::new("session"),
                "test",
                vec![
                    ContextStateMutation::PinFile {
                        path: closed.clone(),
                        scope: PinnedFileScope::Host,
                    },
                    ContextStateMutation::PinFile {
                        path: retained.clone(),
                        scope: PinnedFileScope::Host,
                    },
                ],
            )
            .await
            .unwrap();
        let snapshots = vec![
            FileContextSnapshot {
                path: closed.clone(),
                opened_event: event.id.clone(),
                anchor: MessageId::new("message"),
                text: "closed-file-sensitive-contents".into(),
            },
            FileContextSnapshot {
                path: retained,
                opened_event: event.id.clone(),
                anchor: MessageId::new("message"),
                text: "retained-file-contents".into(),
            },
        ];
        store
            .save_snapshots(
                &SessionId::new("session"),
                Some(&event.id),
                snapshots.clone(),
                vec![],
                &HashSet::new(),
            )
            .await
            .unwrap();
        let mutations = vec![ContextStateMutation::UnpinFile {
            path: closed.clone(),
            reason: None,
        }];
        match source {
            "command" => {
                store
                    .append_command(
                        &SessionId::new("session"),
                        &ScriptExecutionId::new("execution"),
                        0,
                        &CommandInvocationId::new("invocation"),
                        "kraai-close-files",
                        mutations,
                    )
                    .await
                    .unwrap();
            }
            "runtime" => {
                store
                    .append_runtime(&SessionId::new("session"), "test", mutations)
                    .await
                    .unwrap();
            }
            _ => {
                store
                    .save_snapshots(
                        &SessionId::new("session"),
                        Some(&event.id),
                        snapshots.clone(),
                        mutations,
                        &HashSet::new(),
                    )
                    .await
                    .unwrap();
            }
        }
        let restarted = SqliteContextStateStore::new(&root);
        let saved = restarted.load(&SessionId::new("session")).await.unwrap();
        assert!(saved.snapshots == snapshots.get(1..).unwrap_or_default());
        assert!(
            saved
                .pinned_files()
                .iter()
                .map(|file| &file.path)
                .eq(snapshots.get(1).map(|snapshot| &snapshot.path))
        );
        assert!(
            store
                .save_snapshots(
                    &SessionId::new("session"),
                    Some(&event.id),
                    snapshots.clone(),
                    vec![],
                    &HashSet::new()
                )
                .await
                .is_err()
        );
        store
            .append_runtime(
                &SessionId::new("session"),
                "test",
                vec![ContextStateMutation::PinFile {
                    path: closed,
                    scope: PinnedFileScope::Host,
                }],
            )
            .await
            .unwrap();
        assert!(store.snapshots(&SessionId::new("session")).await.unwrap() == saved.snapshots);
        fs::remove_dir_all(root).await.unwrap();
    }
}

#[tokio::test]
async fn snapshot_commit_checks_event_revision_and_atomically_applies_removals() {
    let root = std::env::temp_dir().join(format!("file-context-{}", Ulid::generate()));
    fs::create_dir(&root).await.unwrap();
    let store = SqliteContextStateStore::new(&root);
    let path = root.join("file.txt");
    let event = store
        .append_runtime(
            &SessionId::new("session"),
            "test",
            vec![ContextStateMutation::PinFile {
                path: path.clone(),
                scope: PinnedFileScope::Host,
            }],
        )
        .await
        .unwrap();
    let snapshot = FileContextSnapshot {
        path: path.clone(),
        opened_event: event.id.clone(),
        anchor: MessageId::new("message"),
        text: "original\r\n".into(),
    };
    store
        .save_snapshots(
            &SessionId::new("session"),
            Some(&event.id),
            vec![snapshot.clone()],
            vec![],
            &HashSet::new(),
        )
        .await
        .unwrap();
    let reopened = SqliteContextStateStore::new(&root);
    assert!(
        reopened
            .snapshots(&SessionId::new("session"))
            .await
            .unwrap()
            == vec![snapshot.clone()]
    );
    let later = store
        .append_runtime(
            &SessionId::new("session"),
            "test",
            vec![ContextStateMutation::PinFile {
                path: path.clone(),
                scope: PinnedFileScope::Host,
            }],
        )
        .await
        .unwrap();
    let removals = vec![ContextStateMutation::UnpinFile {
        path,
        reason: Some("missing".into()),
    }];
    assert!(
        store
            .save_snapshots(
                &SessionId::new("session"),
                Some(&event.id),
                vec![],
                removals.clone(),
                &HashSet::new()
            )
            .await
            .is_err()
    );
    assert!(store.snapshots(&SessionId::new("session")).await.unwrap() == vec![snapshot.clone()]);
    assert!(store.list(&SessionId::new("session")).await.unwrap().len() == 1);
    store
        .save_snapshots(
            &SessionId::new("session"),
            Some(&later.id),
            vec![],
            removals.clone(),
            &HashSet::new(),
        )
        .await
        .unwrap();
    assert!(
        store
            .snapshots(&SessionId::new("session"))
            .await
            .unwrap()
            .is_empty()
    );
    let events = store.list(&SessionId::new("session")).await.unwrap();
    assert!(
        events
            .last()
            .is_some_and(|event| event.mutations == removals)
    );
    store.delete(&SessionId::new("session")).await.unwrap();
    assert!(
        store
            .list(&SessionId::new("session"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .snapshots(&SessionId::new("session"))
            .await
            .unwrap()
            .is_empty()
    );
    fs::remove_dir_all(root).await.unwrap();
}

fn event(id: &str, message: Option<&str>, mutation: ContextStateMutation) -> ContextStateEvent {
    ContextStateEvent {
        id: id.into(),
        source: ContextStateEventSource::Runtime {
            component: "test".into(),
        },
        mutations: vec![mutation],
        message_id: message.map(MessageId::new),
    }
}

fn pin(path: &str, scope: PinnedFileScope) -> ContextStateMutation {
    ContextStateMutation::PinFile {
        path: path.into(),
        scope,
    }
}

fn unpin(path: &str) -> ContextStateMutation {
    ContextStateMutation::UnpinFile {
        path: path.into(),
        reason: None,
    }
}

fn pinned(document: &ContextStateDocument) -> Vec<(String, PinnedFileScope, String)> {
    document
        .pinned_files()
        .into_iter()
        .map(|file| {
            (
                file.path.display().to_string(),
                file.scope,
                file.opened_event,
            )
        })
        .collect()
}

#[test]
fn pins_keep_their_first_event_take_the_latest_scope_and_unpin_in_order() {
    let mut document = ContextStateDocument::default();
    let workspace = PinnedFileScope::Workspace {
        root: PathBuf::from("/workspace"),
    };
    document.push_event(event("e1", None, pin("a", workspace)));
    document.push_event(event("e2", None, pin("a", PinnedFileScope::Host)));
    document.push_event(event("e3", None, pin("b", PinnedFileScope::Host)));
    assert_eq!(
        pinned(&document),
        vec![
            ("a".into(), PinnedFileScope::Host, "e1".into()),
            ("b".into(), PinnedFileScope::Host, "e3".into()),
        ]
    );
    document.push_event(event("e4", None, unpin("a")));
    assert_eq!(
        pinned(&document),
        vec![("b".into(), PinnedFileScope::Host, "e3".into())]
    );
}

#[test]
fn folding_keeps_the_state_and_the_revertible_tail() {
    let mut document = ContextStateDocument::default();
    document.push_event(event("e1", Some("old"), pin("a", PinnedFileScope::Host)));
    document.push_event(event("e2", Some("recent"), pin("b", PinnedFileScope::Host)));
    document.push_event(event("e3", None, unpin("a")));
    document.push_event(event("e4", Some("recent"), unpin("b")));
    let state = pinned(&document);
    let recent = HashSet::from([MessageId::new("recent")]);
    assert!(document.fold(&recent));
    assert!(!document.fold(&recent));
    assert_eq!(pinned(&document), state);
    assert_eq!(
        document
            .events
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        ["e2", "e3", "e4"]
    );
    assert!(document.fold(&HashSet::new()));
    assert!(document.events.is_empty());
    assert_eq!(pinned(&document), state);
}

#[test]
fn reverting_messages_restores_earlier_pins_and_keeps_only_their_snapshots() {
    let mut document = ContextStateDocument::default();
    document.push_event(event("e1", Some("first"), pin("a", PinnedFileScope::Host)));
    document.push_event(event("e2", Some("second"), unpin("a")));
    document.push_event(event("e3", Some("second"), pin("b", PinnedFileScope::Host)));
    document.snapshots = ["a", "b"]
        .into_iter()
        .zip(["e1", "e3"])
        .map(|(path, opened)| FileContextSnapshot {
            path: path.into(),
            opened_event: opened.into(),
            anchor: MessageId::new("second"),
            text: String::new(),
        })
        .collect();
    let revision = document.revision.clone();
    assert!(!document.revert(&HashSet::from([MessageId::new("other")])));
    assert!(document.revert(&HashSet::from([MessageId::new("second")])));
    assert_ne!(document.revision, revision);
    assert_eq!(
        pinned(&document),
        vec![("a".into(), PinnedFileScope::Host, "e1".into())]
    );
    assert!(
        document
            .snapshots
            .iter()
            .map(|snapshot| snapshot.path.as_path())
            .eq([Path::new("a")])
    );
}
