use std::sync::Mutex;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use tokio::sync::{mpsc, oneshot, watch};

use super::{Command, RuntimeHandle};
use crate::{RuntimeError, RuntimeResult};

type Shutdown = Shared<BoxFuture<'static, RuntimeResult<()>>>;

#[derive(Default)]
struct LifecycleState {
    thread: Option<std::thread::JoinHandle<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    shutdown: Option<Shutdown>,
}

pub(crate) struct RuntimeLifecycle {
    shutdown_tx: watch::Sender<bool>,
    state: Mutex<LifecycleState>,
}

impl RuntimeLifecycle {
    pub(crate) fn new(shutdown_tx: watch::Sender<bool>) -> Self {
        Self {
            shutdown_tx,
            state: Mutex::new(LifecycleState::default()),
        }
    }

    pub(crate) fn set_thread(&self, thread: std::thread::JoinHandle<()>) {
        if let Ok(mut state) = self.state.lock() {
            state.thread = Some(thread);
        }
    }

    pub(crate) fn set_task(&self, task: tokio::task::JoinHandle<()>) {
        if let Ok(mut state) = self.state.lock() {
            state.task = Some(task);
        }
    }

    pub(super) async fn shutdown(&self, command_tx: &mpsc::Sender<Command>) -> RuntimeResult<()> {
        let shutdown = {
            let mut state = self.state.lock().map_err(RuntimeError::internal)?;
            if let Some(shutdown) = &state.shutdown {
                shutdown.clone()
            } else {
                let command_tx = command_tx.clone();
                let thread = state.thread.take();
                let task = state.task.take();
                let shutdown = async move {
                    let response_result = request_shutdown(&command_tx).await;
                    let thread_result = if let Some(thread) = thread {
                        tokio::task::spawn_blocking(move || thread.join())
                            .await
                            .map_err(RuntimeError::internal)
                            .and_then(|result| {
                                result.map_err(|_panic| {
                                    RuntimeError::internal("runtime background thread panicked")
                                })
                            })
                    } else {
                        Ok(())
                    };
                    let task_result = if let Some(task) = task {
                        task.await.map_err(|error| {
                            RuntimeError::internal(format!("runtime task failed: {error}"))
                        })
                    } else {
                        Ok(())
                    };
                    response_result?;
                    thread_result?;
                    task_result
                }
                .boxed()
                .shared();
                state.shutdown = Some(shutdown.clone());
                shutdown
            }
        };
        shutdown.await
    }
}

impl Drop for RuntimeLifecycle {
    fn drop(&mut self) {
        self.shutdown_tx.send_replace(true);
    }
}

pub(super) async fn request_shutdown(command_tx: &mpsc::Sender<Command>) -> RuntimeResult<()> {
    let (tx, rx) = oneshot::channel();
    if command_tx
        .send(Command::Shutdown { response: Some(tx) })
        .await
        .is_ok()
    {
        rx.await
            .map_err(|_error| RuntimeHandle::response_channel_closed())??;
    }
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests directly inspect shutdown channel and task coordination"
)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::RuntimeStartupState;
    use crate::handle::RuntimeEventSender;

    fn handle() -> (
        RuntimeHandle,
        mpsc::Receiver<Command>,
        watch::Receiver<bool>,
    ) {
        let (command_tx, command_rx) = mpsc::channel(1);
        let (_startup_tx, startup_rx) = watch::channel(RuntimeStartupState::Ready);
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
        tokio::time::timeout(Duration::from_secs(1), retry)
            .await
            .unwrap()
            .unwrap();
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

        let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(first, second)
        })
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
        tokio::time::timeout(Duration::from_secs(1), retry)
            .await
            .unwrap()
            .unwrap();
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

        let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(first, second)
        })
        .await
        .unwrap();
        let expected = Err(RuntimeHandle::response_channel_closed());
        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert!(commands.try_recv().is_err());
    }
}
