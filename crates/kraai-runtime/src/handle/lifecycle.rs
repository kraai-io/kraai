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
mod tests;
