use kraai_types::SessionId;
use std::collections::HashMap;
use std::sync::Mutex;

use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;
use tokio_util::task::task_tracker::TaskTrackerToken;

#[derive(Clone)]
pub(super) struct SessionTaskToken {
    _all: TaskTrackerToken,
    _session: TaskTrackerToken,
    _auxiliary: Option<TaskTrackerToken>,
}

#[derive(Default)]
pub(super) struct StreamTasks {
    all: TaskTracker,
    sessions: Mutex<HashMap<SessionId, TaskTracker>>,
    auxiliary: Mutex<HashMap<SessionId, TaskTracker>>,
}

impl StreamTasks {
    pub(super) fn spawn<F>(&self, session_id: &SessionId, future: F) -> JoinHandle<F::Output>
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

    pub(super) fn session_token(&self, session_id: &SessionId) -> SessionTaskToken {
        let session = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(session_id.clone())
            .or_default()
            .token();
        SessionTaskToken {
            _all: self.all.token(),
            _session: session,
            _auxiliary: None,
        }
    }

    pub(super) fn auxiliary_token(&self, session_id: &SessionId) -> SessionTaskToken {
        let mut token = self.session_token(session_id);
        token._auxiliary = Some(
            self.auxiliary
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entry(session_id.clone())
                .or_default()
                .token(),
        );
        token
    }

    pub(super) async fn wait_session(&self, session_id: &SessionId) {
        Self::wait_tracked_session(&self.sessions, session_id).await;
    }

    pub(super) async fn wait_auxiliary_session(&self, session_id: &SessionId) {
        Self::wait_tracked_session(&self.auxiliary, session_id).await;
    }

    async fn wait_tracked_session(
        trackers: &Mutex<HashMap<SessionId, TaskTracker>>,
        session_id: &SessionId,
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
}

#[cfg(test)]
mod tests;
