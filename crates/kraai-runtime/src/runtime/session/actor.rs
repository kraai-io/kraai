use std::collections::VecDeque;

use kraai_agent::PendingStreamRequest;
use kraai_execution_types::ScriptExecutionId;
use kraai_provider_core::ProviderManager;
use kraai_types::{MessageContent, MessageId, ModelSelection, SessionId};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::super::core::RuntimeCore;
use super::super::script_execution::{CompletedScriptExecution, PendingScriptApproval};
use super::super::stream_driver::StreamDriveResult;
use super::stream::StreamEvent;
use crate::api::Event;
use crate::{
    ContinueSessionOutcome, PendingScriptInfo, RuntimeError, RuntimeResult, SessionActivity,
    SessionSnapshot, SubmitMessageOutcome,
};

pub(in crate::runtime) type Responder<T> = oneshot::Sender<RuntimeResult<T>>;

pub(crate) enum SessionMessage {
    Send {
        message: MessageContent,
        selection: ModelSelection,
        response: Responder<SubmitMessageOutcome>,
    },
    Continue {
        selection: Option<ModelSelection>,
        wait_for_preparation: bool,
        response: Responder<ContinueSessionOutcome>,
    },
    Cancel {
        discard_queue: bool,
        expected_lease: Option<i64>,
        response: Option<Responder<bool>>,
    },
    Approve {
        execution_id: ScriptExecutionId,
        response: Responder<()>,
    },
    Deny {
        execution_id: ScriptExecutionId,
        response: Responder<()>,
    },
    PendingScript {
        response: Responder<Option<PendingScriptInfo>>,
    },
    Snapshot {
        response: Responder<SessionSnapshot>,
    },
    SetMcpServers {
        config: kraai_mcp::McpConfig,
        response: Responder<()>,
    },
    Undo {
        response: Responder<Option<MessageContent>>,
    },
    Delete {
        response: Responder<()>,
    },
    Stop {
        response: oneshot::Sender<()>,
    },
    Prepared {
        generation: u64,
        outcome: kraai_agent::AgentResult<Option<(ProviderManager, PendingStreamRequest)>>,
    },
    Stream {
        generation: u64,
        event: StreamEvent,
    },
    StreamEnded {
        generation: u64,
        result: StreamDriveResult,
    },
    ScriptEnded {
        generation: u64,
        result: color_eyre::Result<CompletedScriptExecution>,
    },
    /// Runs test code inside the actor, ordered with the session's other messages.
    #[cfg(test)]
    Hook(ActorHook),
}

#[cfg(test)]
pub(in crate::runtime) type ActorHook = Box<
    dyn for<'a> FnOnce(&'a mut SessionActor) -> futures::future::BoxFuture<'a, ()> + Send + Sync,
>;

impl SessionMessage {
    /// Answers a request that can no longer reach a running actor.
    pub(crate) fn reject(self) {
        let error = || RuntimeError::unavailable("Session actor stopped");
        match self {
            Self::Send { response, .. } => drop(response.send(Err(error()))),
            Self::Continue { response, .. } => drop(response.send(Err(error()))),
            Self::Cancel {
                response: Some(response),
                ..
            } => drop(response.send(Err(error()))),
            Self::Approve { response, .. } | Self::Deny { response, .. } => {
                drop(response.send(Err(error())));
            }
            Self::PendingScript { response } => drop(response.send(Err(error()))),
            Self::Snapshot { response } => drop(response.send(Err(error()))),
            Self::SetMcpServers { response, .. } => drop(response.send(Err(error()))),
            Self::Undo { response } => drop(response.send(Err(error()))),
            Self::Delete { response } => drop(response.send(Err(error()))),
            Self::Stop { response } => drop(response.send(())),
            Self::Cancel { response: None, .. }
            | Self::Prepared { .. }
            | Self::Stream { .. }
            | Self::StreamEnded { .. }
            | Self::ScriptEnded { .. } => {}
            #[cfg(test)]
            Self::Hook(_) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    Idle,
    Preparing,
    Streaming,
    AwaitingApproval,
    RunningScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamKind {
    Initial,
    Continuation,
}

/// Who is waiting for a preparation to finish.
pub(super) enum PreparationOrigin {
    Send(Responder<SubmitMessageOutcome>),
    Continue(Option<Responder<ContinueSessionOutcome>>),
    Queue,
}

pub(super) struct Preparation {
    pub(super) generation: u64,
    pub(super) kind: StreamKind,
    pub(super) origin: PreparationOrigin,
    pub(super) selection: Option<ModelSelection>,
    pub(super) intercepted: bool,
    pub(super) retried: bool,
    pub(super) cancel: Option<CancelRequest>,
}

pub(super) struct CancelRequest {
    pub(super) discarded: bool,
    pub(super) waiters: Vec<Responder<bool>>,
}

pub(super) struct StreamJob {
    pub(super) generation: u64,
    pub(super) kind: StreamKind,
    pub(super) message_id: MessageId,
    pub(super) cancel: CancellationToken,
    pub(super) task: JoinHandle<()>,
}

pub(super) struct ScriptJob {
    pub(super) generation: u64,
    pub(super) cancel: CancellationToken,
    pub(super) cancel_waiters: Vec<Responder<bool>>,
}

pub(super) enum TurnState {
    Idle,
    Preparing(Preparation),
    Streaming(StreamJob),
    AwaitingApproval(Box<PendingScriptApproval>),
    RunningScript(ScriptJob),
}

impl From<ActivityKind> for SessionActivity {
    fn from(activity: ActivityKind) -> Self {
        match activity {
            ActivityKind::Idle | ActivityKind::Preparing => Self::Idle,
            ActivityKind::Streaming => Self::Streaming,
            ActivityKind::AwaitingApproval => Self::AwaitingApproval,
            ActivityKind::RunningScript => Self::ExecutingScript,
        }
    }
}

impl TurnState {
    pub(super) fn kind(&self) -> ActivityKind {
        match self {
            Self::Idle => ActivityKind::Idle,
            Self::Preparing(_) => ActivityKind::Preparing,
            Self::Streaming(_) => ActivityKind::Streaming,
            Self::AwaitingApproval(_) => ActivityKind::AwaitingApproval,
            Self::RunningScript(_) => ActivityKind::RunningScript,
        }
    }
}

pub(in crate::runtime) struct SessionActor {
    pub(in crate::runtime) runtime: RuntimeCore,
    pub(in crate::runtime) session_id: SessionId,
    pub(super) sender: mpsc::UnboundedSender<SessionMessage>,
    pub(super) state: TurnState,
    pub(super) next_generation: u64,
    /// Requests that must wait until the current preparation finishes.
    pub(super) deferred: VecDeque<SessionMessage>,
    pub(super) stopping: bool,
    stopped: Vec<oneshot::Sender<()>>,
    /// The state clients last heard about.
    published: (SessionActivity, bool),
}

pub(super) fn spawn(
    runtime: RuntimeCore,
    session_id: SessionId,
    tasks: &TaskTracker,
) -> mpsc::UnboundedSender<SessionMessage> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let actor = SessionActor {
        runtime,
        session_id,
        sender: sender.clone(),
        state: TurnState::Idle,
        next_generation: 0,
        deferred: VecDeque::new(),
        stopping: false,
        stopped: Vec::new(),
        published: (SessionActivity::Idle, false),
    };
    tasks.spawn(actor.run(receiver));
    sender
}

impl SessionActor {
    async fn run(mut self, mut receiver: mpsc::UnboundedReceiver<SessionMessage>) {
        while let Some(message) = receiver.recv().await {
            let mut exit = self.handle(message).await;
            while !exit
                && !matches!(self.state, TurnState::Preparing(_))
                && let Some(message) = self.deferred.pop_front()
            {
                exit = self.handle(message).await;
            }
            // After the message's events, so clients that stop at idle already saw how a turn ended.
            self.publish_state();
            if exit || (self.stopping && !self.has_child()) {
                break;
            }
        }
        for stopped in self.stopped.drain(..) {
            let _ = stopped.send(());
        }
        for message in self.deferred.drain(..) {
            message.reject();
        }
        self.runtime.sessions.remove(&self.session_id, &self.sender);
        receiver.close();
        while let Ok(message) = receiver.try_recv() {
            message.reject();
        }
    }

    /// Returns true when the actor should exit.
    async fn handle(&mut self, message: SessionMessage) -> bool {
        match message {
            SessionMessage::Send {
                message,
                selection,
                response,
            } => self.send(message, selection, response).await,
            SessionMessage::Continue {
                selection,
                wait_for_preparation,
                response,
            } => {
                if wait_for_preparation && matches!(self.state, TurnState::Preparing(_)) {
                    self.deferred.push_back(SessionMessage::Continue {
                        selection,
                        wait_for_preparation,
                        response,
                    });
                } else {
                    self.continue_session(selection, Some(response)).await;
                }
            }
            SessionMessage::Cancel {
                discard_queue,
                expected_lease,
                response,
            } => self.cancel(discard_queue, expected_lease, response).await,
            SessionMessage::Approve {
                execution_id,
                response,
            } => {
                let result = self.approve(&execution_id);
                let _ = response.send(result.map_err(RuntimeError::from));
            }
            SessionMessage::Deny {
                execution_id,
                response,
            } => {
                let result = self.deny(&execution_id).await;
                let _ = response.send(result.map_err(RuntimeError::from));
            }
            SessionMessage::PendingScript { response } => {
                let _ = response.send(Ok(self.pending_script_info()));
            }
            SessionMessage::Snapshot { response } => self.snapshot(response).await,
            SessionMessage::SetMcpServers { config, response } => {
                let result = self.set_mcp_servers(config).await;
                let _ = response.send(result.map_err(RuntimeError::from));
            }
            SessionMessage::Undo { response } => {
                let result = self
                    .runtime
                    .agent_manager
                    .undo_last_user_message(&self.session_id)
                    .await;
                let _ = response.send(result.map_err(RuntimeError::from));
            }
            SessionMessage::Delete { response } => return self.delete(response).await,
            SessionMessage::Stop { response } => {
                self.stopped.push(response);
                self.stop();
            }
            SessionMessage::Prepared {
                generation,
                outcome,
            } => self.prepared(generation, outcome).await,
            SessionMessage::Stream { generation, event } => {
                self.stream_event(generation, event).await;
            }
            SessionMessage::StreamEnded { generation, result } => {
                self.stream_ended(generation, result).await;
            }
            SessionMessage::ScriptEnded { generation, result } => {
                self.script_ended(generation, result).await;
            }
            #[cfg(test)]
            SessionMessage::Hook(hook) => hook(self).await,
        }
        false
    }

    fn publish_state(&mut self) {
        let activity = self.state.kind();
        self.runtime
            .sessions
            .publish_activity(&self.session_id, activity);
        let running = activity != ActivityKind::Idle
            || self.runtime.agent_manager.is_turn_active(&self.session_id);
        let state = (SessionActivity::from(activity), running);
        if state == self.published {
            return;
        }
        self.published = state;
        self.runtime.send_event(Event::SessionStateChanged {
            session_id: self.session_id.clone(),
            activity: state.0,
            is_running: state.1,
        });
    }

    pub(super) fn generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1);
        self.next_generation
    }

    /// Sends a message to this actor from one of its child tasks.
    pub(super) fn mailbox(&self) -> mpsc::UnboundedSender<SessionMessage> {
        self.sender.clone()
    }

    pub(super) async fn finish_turn(&mut self) {
        self.state = TurnState::Idle;
        self.runtime.finish_turn(&self.session_id).await;
    }

    fn has_child(&self) -> bool {
        matches!(
            self.state,
            TurnState::Preparing(_) | TurnState::Streaming(_) | TurnState::RunningScript(_)
        )
    }

    /// Stops child work; the actor exits once every child has reported back.
    fn stop(&mut self) {
        self.stopping = true;
        match &self.state {
            TurnState::Streaming(job) => job.cancel.cancel(),
            TurnState::RunningScript(job) => job.cancel.cancel(),
            TurnState::Idle | TurnState::Preparing(_) | TurnState::AwaitingApproval(_) => {}
        }
    }
}
