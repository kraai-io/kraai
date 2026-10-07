#![expect(
    clippy::panic_in_result_fn,
    reason = "bootstrap tests assert filesystem and durability outcomes"
)]

use std::fs;

use super::*;

#[test]
fn independent_initializers_sync_existing_and_overlapping_directories_to_the_anchor()
-> io::Result<()> {
    let root = tempfile::tempdir()?;
    let first_path = root.path().join("shared/first");
    let first = DirectoryBootstrap::new(root.path().to_path_buf(), first_path.clone());
    assert!(
        first
            .create_private_with_sync(|directory| {
                if directory == root.path() {
                    Err(io::Error::other("injected ancestor sync failure"))
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    drop(first);
    for path in [first_path, root.path().join("shared/second")] {
        let bootstrap = DirectoryBootstrap::new(root.path().to_path_buf(), path.clone());
        let mut synced = Vec::new();
        bootstrap.create_private_with_sync(|directory| {
            synced.push(directory.to_path_buf());
            sync_directory(directory)
        })?;
        assert_eq!(
            synced,
            vec![path, root.path().join("shared"), root.path().to_path_buf()]
        );
    }
    Ok(())
}

#[test]
fn recreated_directory_sync_failure_revokes_prior_ready_state() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("credentials");
    let bootstrap = DirectoryBootstrap::new(root.path().to_path_buf(), path.clone());
    bootstrap.create_private()?;
    fs::remove_dir(&path)?;
    for _ in 0..2 {
        assert!(
            bootstrap
                .create_private_with_sync(|directory| {
                    if directory == root.path() {
                        Err(io::Error::other("injected ancestor sync failure"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        assert!(path.is_dir());
    }
    bootstrap.create_private()?;
    Ok(())
}

#[test]
fn failed_sync_retains_original_anchor_until_every_directory_is_durable() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("parent/credentials");
    let bootstrap = DirectoryBootstrap::new(root.path().to_path_buf(), path.clone());
    for _ in 0..2 {
        let mut synced = Vec::new();
        let result = bootstrap.create_private_with_sync(|directory| {
            synced.push(directory.to_path_buf());
            if directory == root.path() {
                Err(io::Error::other("injected ancestor sync failure"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert!(path.is_dir());
        assert_eq!(
            synced,
            vec![
                path.clone(),
                root.path().join("parent"),
                root.path().to_path_buf()
            ]
        );
    }
    let mut synced = Vec::new();
    bootstrap.create_private_with_sync(|directory| {
        synced.push(directory.to_path_buf());
        sync_directory(directory)
    })?;
    assert_eq!(
        synced,
        vec![
            path.clone(),
            root.path().join("parent"),
            root.path().to_path_buf()
        ]
    );
    bootstrap.create_private_with_sync(|_directory| Err(io::Error::other("already durable")))?;
    fs::remove_dir_all(root.path().join("parent"))?;
    bootstrap.create_private()?;
    assert!(path.is_dir());
    Ok(())
}

#[cfg(feature = "async")]
#[tokio::test]
async fn cancelled_waiter_does_not_discard_failed_bootstrap_anchor() -> io::Result<()> {
    use std::sync::Arc;

    let root = tempfile::tempdir()?;
    let path = root.path().join("parent/credentials");
    let bootstrap = Arc::new(DirectoryBootstrap::new(
        root.path().to_path_buf(),
        path.clone(),
    ));
    let creating = bootstrap.clone();
    let anchor = root.path().to_path_buf();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || {
            let mut entered_tx = Some(entered_tx);
            let mut release_rx = Some(release_rx);
            let result = creating.create_private_with_sync(|directory| {
                if directory == anchor {
                    if let Some(sender) = entered_tx.take() {
                        let _ = sender.send(());
                    }
                    if let Some(receiver) = release_rx.take() {
                        receiver.blocking_recv().map_err(io::Error::other)?;
                    }
                    Err(io::Error::other("injected ancestor sync failure"))
                } else {
                    Ok(())
                }
            });
            let _ = finished_tx.send(result);
        })
        .await
    });
    entered_rx.await.map_err(io::Error::other)?;
    waiter.abort();
    assert!(waiter.await.is_err_and(|error| error.is_cancelled()));
    release_tx
        .send(())
        .map_err(|()| io::Error::other("worker stopped"))?;
    assert!(finished_rx.await.map_err(io::Error::other)?.is_err());
    let mut synced = Vec::new();
    bootstrap.create_private_with_sync(|directory| {
        synced.push(directory.to_path_buf());
        sync_directory(directory)
    })?;
    assert_eq!(
        synced,
        vec![path, root.path().join("parent"), root.path().to_path_buf()]
    );
    Ok(())
}
