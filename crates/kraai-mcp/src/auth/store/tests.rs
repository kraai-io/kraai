use kraai_test_support::HANG_TIMEOUT;

use super::*;

#[tokio::test]
async fn cancelled_commit_keeps_lock_until_blocking_write_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth/credentials.json");
    let store = FileStore::current(
        path.clone(),
        Arc::new(Storage::new(
            kraai_io::fs::DirectoryBootstrap::new(
                directory.path().to_path_buf(),
                directory.path().join("auth"),
            ),
            HANG_TIMEOUT,
        )),
    )
    .unwrap()
    .reset()
    .await
    .unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let committing = store.clone();
    let caller = tokio::spawn(async move {
        committing
            .commit_locked(move |store| {
                let _ = entered_tx.send(());
                release_rx.blocking_recv().map_err(error)?;
                store.write(&Document {
                    generation: store.generation,
                    registration_secret: Some(String::from("committed")),
                    ..Document::default()
                })
            })
            .await
    });
    entered_rx.await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let contender = open_private_lock_file(&path.with_extension("lock")).unwrap();
    assert!(matches!(
        contender.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    tokio::time::sleep(Duration::from_millis(1)).await;
    release_tx.send(()).unwrap();
    let guard = store.lock().await.unwrap();
    assert_eq!(
        store.registration_secret().unwrap().as_deref(),
        Some("committed")
    );
    drop(guard);
    drop(contender);
}
