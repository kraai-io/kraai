use super::*;
use kraai_types::PinnedFileScope;

#[tokio::test]
async fn snapshot_commit_checks_event_revision_and_atomically_applies_removals() -> Result<()> {
    let root = std::env::temp_dir().join(format!("file-context-{}", Ulid::generate()));
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
