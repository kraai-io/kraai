use super::*;
use kraai_types::PinnedFileScope;
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
                    )
                    .await
                    .unwrap();
            }
        }
        let restarted = SqliteContextStateStore::new(&root);
        let saved = restarted.load(&SessionId::new("session")).await.unwrap();
        assert!(saved.snapshots == snapshots.get(1..).unwrap_or_default());
        assert!(saved.events.len() == 2);
        assert!(
            store
                .save_snapshots(
                    &SessionId::new("session"),
                    Some(&event.id),
                    snapshots.clone(),
                    vec![]
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
                removals.clone()
            )
            .await
            .is_err()
    );
    assert!(store.snapshots(&SessionId::new("session")).await.unwrap() == vec![snapshot.clone()]);
    assert!(store.list(&SessionId::new("session")).await.unwrap().len() == 2);
    store
        .save_snapshots(
            &SessionId::new("session"),
            Some(&later.id),
            vec![],
            removals.clone(),
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
