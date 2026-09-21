#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests directly inspect request channel coordination"
)]

use super::*;

fn handle() -> (RuntimeHandle, mpsc::Receiver<Command>) {
    let (command_tx, command_rx) = mpsc::channel(1);
    let (_startup_tx, startup_rx) = tokio::sync::watch::channel(RuntimeStartupState::Ready);
    (
        RuntimeHandle {
            command_tx,
            event_tx: RuntimeEventSender::new(1),
            lifecycle: None,
            startup_rx,
        },
        command_rx,
    )
}

#[tokio::test]
async fn closed_channels_report_the_failed_transport_stage() {
    let (client, commands) = handle();
    drop(commands);
    assert_eq!(
        client.list_models().await,
        Err(RuntimeHandle::command_channel_closed())
    );

    let (client, mut commands) = handle();
    let mut request = Box::pin(client.list_models());
    assert!(futures::poll!(&mut request).is_pending());
    let Some(Command::ListModels { response }) = commands.recv().await else {
        panic!("expected model request");
    };
    drop(response);
    assert_eq!(request.await, Err(RuntimeHandle::response_channel_closed()));
}

#[tokio::test]
async fn cancellation_preserves_the_enqueue_boundary() {
    let (client, mut commands) = handle();
    client.command_tx.try_send(Command::LoadConfig).unwrap();
    let mut waiting = Box::pin(client.list_sessions());
    assert!(futures::poll!(&mut waiting).is_pending());
    drop(waiting);
    assert!(matches!(commands.recv().await, Some(Command::LoadConfig)));
    assert!(matches!(
        commands.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    let mut enqueued = Box::pin(client.get_tip("session".into()));
    assert!(futures::poll!(&mut enqueued).is_pending());
    drop(enqueued);
    let Some(Command::GetTip {
        session_id,
        response,
    }) = commands.recv().await
    else {
        panic!("expected tip request");
    };
    assert_eq!(session_id, "session");
    assert!(response.is_closed());
}

#[tokio::test]
async fn invalid_identifiers_fail_before_waiting_for_queue_capacity() {
    let (client, mut commands) = handle();
    client.command_tx.try_send(Command::LoadConfig).unwrap();
    let mut send = Box::pin(client.send_message(
        "session".into(),
        "message".into(),
        String::new(),
        "provider".into(),
    ));
    let std::task::Poll::Ready(Err(error)) = futures::poll!(&mut send) else {
        panic!("invalid model must fail before enqueueing");
    };
    assert_eq!(error.kind, crate::RuntimeErrorKind::InvalidArgument);
    assert!(error.message.starts_with("invalid model_id: "));

    let mut approve = Box::pin(client.approve_script("session".into(), String::new()));
    let std::task::Poll::Ready(Err(error)) = futures::poll!(&mut approve) else {
        panic!("invalid execution must fail before enqueueing");
    };
    assert_eq!(error.kind, crate::RuntimeErrorKind::InvalidArgument);
    assert!(error.message.starts_with("invalid execution_id: "));
    assert!(matches!(commands.recv().await, Some(Command::LoadConfig)));
    assert!(matches!(
        commands.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}
