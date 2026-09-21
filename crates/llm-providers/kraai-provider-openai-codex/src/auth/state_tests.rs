use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::task::{Context, Wake, Waker};

use super::*;

#[tokio::test]
async fn status_reads_and_broadcasts_share_capture_order() {
    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    let mut updates = controller.subscribe();
    let first = controller.get_status().await;
    let emitted = controller.emit_status().await.unwrap();
    let next = controller.get_status().await;
    let received = updates.recv().await.unwrap();
    assert!(first.sequence < emitted.sequence);
    assert!(emitted.sequence < next.sequence);
    assert_eq!(received.sequence, emitted.sequence);
    assert_eq!(first, emitted);
    assert_eq!(emitted, next);

    controller.inner.state.lock().await.error = Some(String::from("new error"));
    let changed = controller.emit_status().await.unwrap();
    assert!(next.sequence < changed.sequence);
    assert_eq!(updates.recv().await.unwrap().sequence, changed.sequence);
    assert_ne!(next, changed);
}

#[tokio::test]
async fn cancelled_auth_commit_leaves_disk_and_memory_unchanged() {
    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    let path = controller.inner.config.auth_path.clone();
    let original = stored_auth("original@example.com", "pro", "workspace_123", unix_now());
    let original_generation = original.generation.clone();
    controller.replace_auth_for_test(original).await.unwrap();
    let next = stored_auth("next@example.com", "pro", "workspace_123", unix_now());
    let next_generation = next.generation.clone();
    let file_lock = acquire_auth_file_lock(path.clone()).await.unwrap();
    let state = controller.inner.state.lock().await;

    let mut commit = Box::pin(controller.persist_auth_locked(next.clone()));
    assert!(futures::poll!(&mut commit).is_pending());
    assert_eq!(
        load_auth_file(&path).unwrap().unwrap().generation,
        original_generation
    );
    drop(commit);
    assert_eq!(state.auth.as_ref().unwrap().generation, original_generation);
    drop(state);

    controller.persist_auth_locked(next).await.unwrap();
    assert_eq!(
        load_auth_file(&path).unwrap().unwrap().generation,
        next_generation
    );
    assert_eq!(
        controller.get_request_auth().await.unwrap().generation,
        next_generation
    );
    drop(file_lock);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn failed_auth_commit_preserves_memory_and_error() {
    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    let path = controller.inner.config.auth_path.clone();
    let original = stored_auth("original@example.com", "pro", "workspace_123", unix_now());
    let generation = original.generation.clone();
    controller.replace_auth_for_test(original).await.unwrap();
    controller.inner.state.lock().await.error = Some(String::from("existing error"));
    let file_lock = acquire_auth_file_lock(path.clone()).await.unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let next = stored_auth("next@example.com", "pro", "workspace_123", unix_now());

    assert!(controller.persist_auth_locked(next).await.is_err());

    let state = controller.inner.state.lock().await;
    assert_eq!(state.auth.as_ref().unwrap().generation, generation);
    assert_eq!(state.error.as_deref(), Some("existing error"));
    drop(state);
    drop(file_lock);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn status_snapshot_cannot_change_before_its_broadcast() {
    struct StatusWake {
        inner: Weak<Inner>,
        notified: AtomicBool,
        state_locked: AtomicBool,
    }

    impl Wake for StatusWake {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            let inner = self.inner.upgrade().unwrap();
            self.state_locked
                .store(inner.state.try_lock().is_err(), Ordering::SeqCst);
            self.notified.store(true, Ordering::SeqCst);
        }
    }

    let Some(controller) = auth_controller_or_skip() else {
        return;
    };
    controller.inner.state.lock().await.error = Some(String::from("status marker"));
    let mut updates = controller.subscribe();
    let mut receive = Box::pin(updates.recv());
    let wake = Arc::new(StatusWake {
        inner: Arc::downgrade(&controller.inner),
        notified: AtomicBool::new(false),
        state_locked: AtomicBool::new(false),
    });
    let waker = Waker::from(wake.clone());
    assert!(
        receive
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );

    let status = controller.emit_status().await.unwrap();

    assert!(wake.notified.load(Ordering::SeqCst));
    assert!(wake.state_locked.load(Ordering::SeqCst));
    assert_eq!(status.error.as_deref(), Some("status marker"));
    assert_eq!(receive.await.unwrap(), status);
    assert!(controller.inner.state.try_lock().is_ok());
}
