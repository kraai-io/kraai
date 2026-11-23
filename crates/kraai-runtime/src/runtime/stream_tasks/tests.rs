use super::*;

#[tokio::test]
async fn terminal_usage_drain_waits_for_auxiliary_commits_without_waiting_for_itself() {
    let tasks = StreamTasks::default();
    let session = SessionId::new("session");
    let main = tasks.session_token(&session);
    let auxiliary = tasks.auxiliary_token(&session);
    let waiting = tasks.wait_auxiliary_session(&session);
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
    let session = SessionId::new("session");
    let task = tasks.spawn(&session, async move { task_release.cancelled().await });
    {
        let waiting = tasks.wait_session(&session);
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
    }
    let waiting = tasks.wait_session(&session);
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
