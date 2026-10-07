use super::*;
use kraai_types::PinnedFileScope;

#[tokio::test]
async fn unpin_commits_remove_only_closed_snapshots_without_a_refresh() -> Result<()> {
    for source in ["command", "runtime", "refresh"] {
        let root = std::env::temp_dir().join(format!("file-context-close-{}", Ulid::generate()));
        fs::create_dir(&root).await?;
        let store = FileContextStateStore::new(&root);
        let closed = root.join("closed.txt");
        let retained = root.join("retained.txt");
        let event = store
            .append_runtime(
                "session",
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
            .await?;
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
            .save_snapshots("session", Some(&event.id), snapshots.clone(), vec![])
            .await?;
        let mutations = vec![ContextStateMutation::UnpinFile {
            path: closed.clone(),
            reason: None,
        }];
        match source {
            "command" => {
                store
                    .append_command(
                        "session",
                        &ScriptExecutionId::new("execution"),
                        0,
                        &CommandInvocationId::new("invocation"),
                        "kraai-close-files",
                        mutations,
                    )
                    .await?;
            }
            "runtime" => {
                store.append_runtime("session", "test", mutations).await?;
            }
            _ => {
                store
                    .save_snapshots("session", Some(&event.id), snapshots.clone(), mutations)
                    .await?;
            }
        }
        let restarted = FileContextStateStore::new(&root);
        let saved = restarted.load("session").await?;
        ensure!(saved.snapshots == snapshots.get(1..).unwrap_or_default());
        ensure!(saved.events.len() == 2);
        ensure!(
            !fs::read_to_string(store.document_path("session")?)
                .await?
                .contains("closed-file-sensitive-contents")
        );
        ensure!(
            store
                .save_snapshots("session", Some(&event.id), snapshots.clone(), vec![])
                .await
                .is_err()
        );
        store
            .append_runtime(
                "session",
                "test",
                vec![ContextStateMutation::PinFile {
                    path: closed,
                    scope: PinnedFileScope::Host,
                }],
            )
            .await?;
        ensure!(store.snapshots("session").await? == saved.snapshots);
        fs::remove_dir_all(root).await?;
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_commit_checks_event_revision_and_atomically_applies_removals() -> Result<()> {
    let root = std::env::temp_dir().join(format!("file-context-{}", Ulid::generate()));
    fs::create_dir(&root).await?;
    let store = FileContextStateStore::new(&root);
    let path = root.join("file.txt");
    let event = store
        .append_runtime(
            "session",
            "test",
            vec![ContextStateMutation::PinFile {
                path: path.clone(),
                scope: PinnedFileScope::Host,
            }],
        )
        .await?;
    let snapshot = FileContextSnapshot {
        path: path.clone(),
        opened_event: event.id.clone(),
        anchor: MessageId::new("message"),
        text: "original\r\n".into(),
    };
    store
        .save_snapshots("session", Some(&event.id), vec![snapshot.clone()], vec![])
        .await?;
    let reopened = FileContextStateStore::new(&root);
    ensure!(reopened.snapshots("session").await? == vec![snapshot.clone()]);
    let later = store
        .append_runtime(
            "session",
            "test",
            vec![ContextStateMutation::PinFile {
                path: path.clone(),
                scope: PinnedFileScope::Host,
            }],
        )
        .await?;
    let removals = vec![ContextStateMutation::UnpinFile {
        path,
        reason: Some("missing".into()),
    }];
    ensure!(
        store
            .save_snapshots("session", Some(&event.id), vec![], removals.clone())
            .await
            .is_err()
    );
    ensure!(store.snapshots("session").await? == vec![snapshot.clone()]);
    ensure!(store.list("session").await?.len() == 2);
    store
        .save_snapshots("session", Some(&later.id), vec![], removals.clone())
        .await?;
    ensure!(store.snapshots("session").await?.is_empty());
    let events = store.list("session").await?;
    ensure!(
        events
            .last()
            .is_some_and(|event| event.mutations == removals)
    );
    store.delete("session").await?;
    ensure!(store.list("session").await?.is_empty());
    ensure!(store.snapshots("session").await?.is_empty());
    fs::remove_dir_all(root).await?;
    Ok(())
}
