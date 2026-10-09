use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::task::{Context, Wake, Waker};

use super::*;

struct PublicationWake {
    session: Arc<Session>,
    notified: AtomicBool,
    saw_locked: AtomicBool,
    generation: AtomicU64,
}

impl Wake for PublicationWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notified.store(true, Ordering::SeqCst);
        self.saw_locked.store(
            self.session.configuration.try_lock().is_err(),
            Ordering::SeqCst,
        );
        self.generation.store(
            self.session.config_generation.load(Ordering::Acquire),
            Ordering::SeqCst,
        );
    }
}

#[tokio::test]
async fn foreground_publication_unlocks_before_waking_the_refresh_worker() {
    let updated = Arc::new(ConfigUpdates::default());
    let session = Arc::new(Session::new(updated.clone()));
    let publication = lock(&session).await;
    let probe = Arc::new(PublicationWake {
        session: session.clone(),
        notified: AtomicBool::new(false),
        saw_locked: AtomicBool::new(false),
        generation: AtomicU64::new(0),
    });
    let waker = Waker::from(probe.clone());
    let mut context = Context::from_waker(&waker);
    let mut notified = Box::pin(updated.wake.notified());
    assert!(notified.as_mut().poll(&mut context).is_pending());
    drop(publication);
    assert!(probe.notified.load(Ordering::SeqCst));
    assert!(!probe.saw_locked.load(Ordering::SeqCst));
    assert_eq!(probe.generation.load(Ordering::SeqCst), 1);
    assert!(notified.as_mut().poll(&mut context).is_ready());
}

#[tokio::test]
async fn waiting_publisher_observes_new_generation_when_the_mutex_unlocks() {
    let updated = Arc::new(ConfigUpdates::default());
    let session = Arc::new(Session::new(updated));
    let publication = lock(&session).await;
    let probe = Arc::new(PublicationWake {
        session: session.clone(),
        notified: AtomicBool::new(false),
        saw_locked: AtomicBool::new(false),
        generation: AtomicU64::new(0),
    });
    let waker = Waker::from(probe.clone());
    let mut context = Context::from_waker(&waker);
    let mut waiting = Box::pin(session.configuration.lock());
    assert!(waiting.as_mut().poll(&mut context).is_pending());
    drop(publication);
    assert!(probe.notified.load(Ordering::SeqCst));
    assert_eq!(probe.generation.load(Ordering::SeqCst), 1);
    assert!(waiting.as_mut().poll(&mut context).is_ready());
}

#[tokio::test]
async fn session_becomes_ready_only_after_its_response_is_sent() {
    let updated = Arc::new(ConfigUpdates::default());
    let session = Arc::new(Session::new(updated.clone()));
    let sent = AtomicBool::new(false);
    let result = PreparedSession {
        response: "response",
        session: session.clone(),
        _publication: lock(&session).await,
    }
    .respond(|response| {
        assert_eq!(response, "response");
        assert!(!session.ready.load(Ordering::Acquire));
        assert!(session.configuration.try_lock().is_err());
        sent.store(true, Ordering::Release);
        Ok(())
    });
    assert!(result.is_ok());
    assert!(sent.load(Ordering::Acquire));
    assert!(session.ready.load(Ordering::Acquire));
    assert!(session.configuration.try_lock().is_ok());
    assert!(futures::poll!(Box::pin(updated.wake.notified())).is_ready());
}

#[tokio::test]
async fn failed_session_response_keeps_the_session_hidden_and_releases_publication() {
    let updated = Arc::new(ConfigUpdates::default());
    let session = Arc::new(Session::new(updated.clone()));
    let result = PreparedSession {
        response: (),
        session: session.clone(),
        _publication: lock(&session).await,
    }
    .respond(|()| Err(crate::error::internal("response failed")));
    assert!(result.is_err());
    assert!(!session.ready.load(Ordering::Acquire));
    assert!(session.configuration.try_lock().is_ok());
    assert!(futures::poll!(Box::pin(updated.wake.notified())).is_ready());
}

#[tokio::test]
async fn cancelled_foreground_update_does_not_leave_refresh_blocked() {
    let updated = Arc::new(ConfigUpdates::default());
    let session = Session::new(updated.clone());
    let mut action = Box::pin(async {
        let publication = lock(&session).await;
        std::future::pending::<()>().await;
        drop(publication);
    });
    assert!(futures::poll!(action.as_mut()).is_pending());
    assert!(session.configuration.try_lock().is_err());
    drop(action);
    assert!(session.configuration.try_lock().is_ok());
    assert!(futures::poll!(Box::pin(updated.wake.notified())).is_ready());
}

#[tokio::test]
async fn foreground_refresh_requests_coalesce_and_background_publication_does_not_retrigger() {
    let updated = Arc::new(ConfigUpdates::default());
    let sessions = [Session::new(updated.clone()), Session::new(updated.clone())];
    for _ in 0..3 {
        for session in &sessions {
            drop(lock(session).await);
        }
    }
    for session in &sessions {
        assert_eq!(session.config_generation.load(Ordering::Acquire), 3);
    }
    assert!(futures::poll!(Box::pin(updated.wake.notified())).is_ready());
    let mut waiting = Box::pin(updated.wake.notified());
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    for session in &sessions {
        drop(session.configuration.lock().await);
    }
    for session in &sessions {
        assert_eq!(session.config_generation.load(Ordering::Acquire), 3);
    }
    assert!(futures::poll!(waiting.as_mut()).is_pending());
}

#[tokio::test]
async fn an_active_turn_does_not_block_configuration_publication() {
    let session = Arc::new(Session::new(Arc::new(ConfigUpdates::default())));
    let active = session.begin_turn();
    assert!(active.is_ok());
    let mut publication = Box::pin(lock(&session));
    assert!(futures::poll!(publication.as_mut()).is_ready());
    assert!(session.turn.try_lock().is_err());
    drop(active);
}

#[tokio::test]
async fn publishing_one_session_does_not_invalidate_another_sessions_refresh() {
    let updated = Arc::new(ConfigUpdates::default());
    let changed = Session::new(updated.clone());
    let idle = Session::new(updated.clone());
    let idle_generation = idle.config_generation.load(Ordering::Acquire);
    let pending_refresh = idle.configuration.lock().await;
    for _ in 0..3 {
        drop(lock(&changed).await);
        assert_eq!(
            idle.config_generation.load(Ordering::Acquire),
            idle_generation
        );
    }
    assert_eq!(changed.config_generation.load(Ordering::Acquire), 3);
    drop(pending_refresh);
    assert_eq!(
        idle.config_generation.load(Ordering::Acquire),
        idle_generation
    );
    assert!(futures::poll!(Box::pin(updated.wake.notified())).is_ready());
    drop(lock(&idle).await);
    assert_eq!(idle.config_generation.load(Ordering::Acquire), 1);
    assert_eq!(changed.config_generation.load(Ordering::Acquire), 3);
}
