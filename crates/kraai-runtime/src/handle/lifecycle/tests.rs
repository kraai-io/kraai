use std::sync::Arc;

use super::*;
use crate::RuntimeStartupState;
use crate::handle::RuntimeEventSender;
use kraai_test_support::hang_guard;

fn handle() -> (
    RuntimeHandle,
    mpsc::Receiver<Command>,
    watch::Receiver<bool>,
) {
    let (command_tx, command_rx) = mpsc::channel(1);
    let (_startup_tx, startup_rx) = watch::channel(RuntimeStartupState::ready());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    (
        RuntimeHandle {
            command_tx,
            event_tx: RuntimeEventSender::new(1),
            lifecycle: Some(Arc::new(RuntimeLifecycle::new(shutdown_tx))),
            startup_rx,
        },
        command_rx,
        shutdown_rx,
    )
}

async fn acknowledge_shutdown(commands: &mut mpsc::Receiver<Command>) {
    let Some(Command::Shutdown {
        response: Some(response),
    }) = commands.recv().await
    else {
        panic!("expected shutdown command");
    };
    response.send(Ok(())).unwrap();
}

#[tokio::test]
async fn cancelled_send_can_be_retried_without_losing_fifo_order() {
    let (handle, mut commands, _shutdown) = handle();
    handle.command_tx.try_send(Command::LoadConfig).unwrap();
    let mut first = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut first).is_pending());
    drop(first);

    assert!(matches!(commands.recv().await, Some(Command::LoadConfig)));
    let mut retry = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut retry).is_pending());
    acknowledge_shutdown(&mut commands).await;
    hang_guard(retry).await.unwrap().unwrap();
    assert!(commands.try_recv().is_err());
}

#[tokio::test]
async fn dropping_handle_after_cancelled_send_still_signals_shutdown() {
    let (handle, _commands, shutdown) = handle();
    handle.command_tx.try_send(Command::LoadConfig).unwrap();
    let mut pending = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut pending).is_pending());
    drop(pending);

    drop(handle);

    assert!(*shutdown.borrow());
}

#[tokio::test]
async fn concurrent_callers_both_wait_for_host_completion() {
    let (handle, mut commands, _shutdown) = handle();
    let (finish_tx, finish_rx) = oneshot::channel();
    handle
        .lifecycle
        .as_ref()
        .unwrap()
        .set_task(tokio::spawn(async move {
            let _ = finish_rx.await;
        }));
    let mut first = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut first).is_pending());
    acknowledge_shutdown(&mut commands).await;
    assert!(futures::poll!(&mut first).is_pending());

    let mut second = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut second).is_pending());
    finish_tx.send(()).unwrap();

    let (first, second) = hang_guard(async { tokio::join!(first, second) })
        .await
        .unwrap();
    first.unwrap();
    second.unwrap();
    assert!(commands.try_recv().is_err());
}

#[tokio::test]
async fn cancelling_host_join_keeps_it_awaitable() {
    let (handle, mut commands, _shutdown) = handle();
    let (finish_tx, finish_rx) = oneshot::channel();
    handle
        .lifecycle
        .as_ref()
        .unwrap()
        .set_task(tokio::spawn(async move {
            let _ = finish_rx.await;
        }));
    let mut first = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut first).is_pending());
    acknowledge_shutdown(&mut commands).await;
    assert!(futures::poll!(&mut first).is_pending());
    drop(first);

    let mut retry = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut retry).is_pending());
    finish_tx.send(()).unwrap();
    hang_guard(retry).await.unwrap().unwrap();
    assert!(commands.try_recv().is_err());
}

#[tokio::test]
async fn failed_response_still_joins_host_for_all_callers() {
    let (handle, mut commands, _shutdown) = handle();
    let (finish_tx, finish_rx) = oneshot::channel();
    handle
        .lifecycle
        .as_ref()
        .unwrap()
        .set_task(tokio::spawn(async move {
            let _ = finish_rx.await;
        }));
    let mut first = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut first).is_pending());
    let Some(Command::Shutdown {
        response: Some(response),
    }) = commands.recv().await
    else {
        panic!("expected shutdown command");
    };
    drop(response);
    assert!(futures::poll!(&mut first).is_pending());

    let mut second = Box::pin(handle.shutdown());
    assert!(futures::poll!(&mut second).is_pending());
    finish_tx.send(()).unwrap();

    let (first, second) = hang_guard(async { tokio::join!(first, second) })
        .await
        .unwrap();
    let expected = Err(RuntimeHandle::response_channel_closed());
    assert_eq!(first, expected);
    assert_eq!(second, expected);
    assert!(commands.try_recv().is_err());
}
