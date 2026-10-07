#![expect(
    clippy::unwrap_used,
    reason = "Lock tests assert fixture operations directly"
)]

use super::*;

#[test]
fn dropping_guard_releases_lock_without_truncating_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lock");
    std::fs::write(&path, b"keep").unwrap();
    let guard = FileLock::acquire(open_private_lock_file(&path).unwrap()).unwrap();
    let contender = open_private_lock_file(&path).unwrap();
    assert!(matches!(
        FileLock::try_acquire(contender),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(guard);
    let acquired = FileLock::try_acquire(open_private_lock_file(&path).unwrap()).unwrap();
    drop(acquired);
    assert_eq!(std::fs::read(path).unwrap(), b"keep");
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_lock_wait_has_deadline_and_cancellation_releases_handle() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lock");
    let guard = FileLock::acquire(open_private_lock_file(&path).unwrap()).unwrap();
    let error = FileLock::acquire_until(
        open_private_lock_file(&path).unwrap(),
        Some(tokio::time::Instant::now() + Duration::from_millis(20)),
        Duration::from_secs(10),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let waiting = tokio::spawn(FileLock::acquire_until(
        open_private_lock_file(&path).unwrap(),
        None,
        Duration::from_millis(10),
    ));
    tokio::task::yield_now().await;
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    drop(guard);
    let acquired = FileLock::acquire_until(
        open_private_lock_file(&path).unwrap(),
        Some(tokio::time::Instant::now() + Duration::from_millis(100)),
        Duration::from_millis(10),
    )
    .await
    .unwrap();
    drop(acquired);
}

#[cfg(feature = "async")]
#[tokio::test]
async fn zero_poll_interval_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let error = FileLock::acquire_until(
        open_private_lock_file(&root.path().join("lock")).unwrap(),
        None,
        Duration::ZERO,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
