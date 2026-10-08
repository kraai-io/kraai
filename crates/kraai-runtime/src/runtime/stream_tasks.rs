use std::collections::HashMap;
use std::sync::Mutex;

use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;
use tokio_util::task::task_tracker::TaskTrackerToken;

pub(super) struct SessionTaskToken {
    _all: TaskTrackerToken,
    _session: TaskTrackerToken,
    _auxiliary: Option<TaskTrackerToken>,
}

#[cfg(test)]
struct PublicationPause {
    entered: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}

#[derive(Default)]
pub(super) struct StreamTasks {
    all: TaskTracker,
    sessions: Mutex<HashMap<String, TaskTracker>>,
    auxiliary: Mutex<HashMap<String, TaskTracker>>,
    #[cfg(test)]
    publication_pause: Mutex<Option<PublicationPause>>,
}

impl StreamTasks {
    pub(super) fn spawn<F>(&self, session_id: &str, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let token = self.session_token(session_id);
        tokio::spawn(async move {
            let _token = token;
            future.await
        })
    }

    pub(super) fn session_token(&self, session_id: &str) -> SessionTaskToken {
        let session = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(session_id.to_string())
            .or_default()
            .token();
        SessionTaskToken {
            _all: self.all.token(),
            _session: session,
            _auxiliary: None,
        }
    }

    pub(super) fn auxiliary_token(&self, session_id: &str) -> SessionTaskToken {
        let mut token = self.session_token(session_id);
        token._auxiliary = Some(
            self.auxiliary
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entry(session_id.to_string())
                .or_default()
                .token(),
        );
        token
    }

    pub(super) async fn wait_session(&self, session_id: &str) {
        Self::wait_tracked_session(&self.sessions, session_id).await;
    }

    pub(super) async fn wait_auxiliary_session(&self, session_id: &str) {
        Self::wait_tracked_session(&self.auxiliary, session_id).await;
    }

    async fn wait_tracked_session(
        trackers: &Mutex<HashMap<String, TaskTracker>>,
        session_id: &str,
    ) {
        let tracker = trackers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(session_id)
            .cloned();
        if let Some(tracker) = tracker {
            tracker.close();
            tracker.wait().await;
            trackers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(session_id);
        }
    }

    pub(super) fn close(&self) {
        self.all.close();
    }

    pub(super) async fn wait(&self) {
        self.all.wait().await;
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    #[cfg(test)]
    pub(super) fn pause_next_publication(
        &self,
    ) -> (
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    ) {
        let entered = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        *self
            .publication_pause
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(PublicationPause {
            entered: entered.clone(),
            release: release.clone(),
        });
        (entered, release)
    }

    #[cfg(test)]
    pub(super) async fn before_publication(&self) {
        let pause = self
            .publication_pause
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(PublicationPause { entered, release }) = pause {
            entered.notify_one();
            release.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminal_usage_drain_waits_for_auxiliary_commits_without_waiting_for_itself() {
        let tasks = StreamTasks::default();
        let main = tasks.session_token("session");
        let auxiliary = tasks.auxiliary_token("session");
        let waiting = tasks.wait_auxiliary_session("session");
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        drop(auxiliary);
        waiting.await;
        tasks.close();
        let all = tasks.wait();
        tokio::pin!(all);
        assert!(futures::poll!(&mut all).is_pending());
        drop(main);
        all.await;
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn cancelled_wait_keeps_session_tasks_registered() {
        let tasks = StreamTasks::default();
        let release = tokio_util::sync::CancellationToken::new();
        let task_release = release.clone();
        let task = tasks.spawn("session", async move { task_release.cancelled().await });
        {
            let waiting = tasks.wait_session("session");
            tokio::pin!(waiting);
            assert!(futures::poll!(&mut waiting).is_pending());
        }
        let waiting = tasks.wait_session("session");
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        release.cancel();
        waiting.await;
        assert!(task.await.is_ok());
        assert!(
            tasks
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }
}
