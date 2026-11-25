use std::pin::Pin;

use agent_client_protocol::{Error, Result, v1::schema as acp};
use kraai_runtime::{
    Event, PendingScriptInfo, RuntimeErrorKind, RuntimeEvent, RuntimeHandle, SessionSnapshot,
    TurnOutcome,
};
use kraai_types::{ConversationItem, Message, MessageContent, MessageId};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::render::Renderer;
use crate::session::Model;
use crate::{content, error};

#[cfg(test)]
mod tests;

pub(crate) struct Start {
    pub(crate) message: MessageContent,
    pub(crate) model: Model,
    pub(crate) token: CancellationToken,
    pub(crate) done: oneshot::Sender<Result<acp::StopReason>>,
}

pub(crate) struct Handle {
    starts: mpsc::UnboundedSender<Start>,
    stop: CancellationToken,
    stopped: oneshot::Receiver<()>,
}

impl Handle {
    pub(crate) fn start(&self, start: Start) -> Result<()> {
        self.starts
            .send(start)
            .map_err(|_closed| error::internal("Session stopped following runtime events"))
    }

    pub(crate) async fn stop(mut self) {
        self.stop.cancel();
        let _ = (&mut self.stopped).await;
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

type PermissionResponse =
    Pin<Box<dyn Future<Output = Result<acp::RequestPermissionResponse>> + Send>>;

struct Permission {
    execution_id: String,
    call_id: String,
    response: Option<PermissionResponse>,
}

struct Own {
    stream: String,
    started: bool,
    token: CancellationToken,
    done: oneshot::Sender<Result<acp::StopReason>>,
    forced: Option<Result<acp::StopReason>>,
}

enum Step {
    Cancel,
    Start(Start),
    Answer(Result<acp::RequestPermissionResponse>),
    Event(std::result::Result<RuntimeEvent, broadcast::error::RecvError>),
}

pub(crate) struct Follower {
    runtime: RuntimeHandle,
    events: broadcast::Receiver<RuntimeEvent>,
    render: Renderer,
    pending: Option<PendingScriptInfo>,
    own: Option<Own>,
    own_user: bool,
    permission: Option<Permission>,
    abort: Option<Error>,
}

impl Follower {
    pub(crate) fn new(
        runtime: RuntimeHandle,
        events: broadcast::Receiver<RuntimeEvent>,
        render: Renderer,
        pending: Option<PendingScriptInfo>,
    ) -> Self {
        Self {
            runtime,
            events,
            render,
            pending,
            own: None,
            own_user: false,
            permission: None,
            abort: None,
        }
    }

    pub(crate) fn spawn(self, release: oneshot::Receiver<()>) -> Result<Handle> {
        let (starts, receiver) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let (stopped_tx, stopped) = oneshot::channel();
        let connection = self.render.connection.clone();
        let token = stop.clone();
        connection.spawn(async move {
            let released = tokio::select! {
                biased;
                () = token.cancelled() => false,
                released = release => released.is_ok(),
            };
            if released {
                self.run(receiver, token).await;
            }
            drop(stopped_tx);
            Ok(())
        })?;
        Ok(Handle {
            starts,
            stop,
            stopped,
        })
    }

    async fn run(mut self, mut starts: mpsc::UnboundedReceiver<Start>, stop: CancellationToken) {
        if let Some(script) = self.pending.take()
            && let Err(error) = self.request(script)
        {
            self.fail(error).await;
        }
        loop {
            let step = tokio::select! {
                biased;
                () = stop.cancelled() => return,
                () = cancelled(self.own.as_ref()) => Step::Cancel,
                Some(start) = starts.recv() => Step::Start(start),
                response = answered(self.permission.as_mut()) => Step::Answer(response),
                received = self.events.recv() => Step::Event(received),
            };
            let result = match step {
                Step::Cancel => {
                    self.abort_own(Ok(acp::StopReason::Cancelled)).await;
                    Ok(())
                }
                Step::Start(start) => {
                    self.start(start).await;
                    Ok(())
                }
                Step::Answer(response) => self.answer(response).await,
                Step::Event(Ok(event)) => self.event(event).await,
                Step::Event(Err(broadcast::error::RecvError::Lagged(_))) => self.sync().await,
                Step::Event(Err(broadcast::error::RecvError::Closed)) => {
                    self.finish_own(Err(closed()));
                    return;
                }
            };
            if let Err(error) = result {
                self.fail(error).await;
            }
            if let Some(error) = self.abort.take() {
                self.abort_own(Err(error)).await;
            }
        }
    }

    async fn fail(&mut self, error: Error) {
        if self.own.is_some() {
            self.abort_own(Err(error)).await;
        } else {
            tracing::warn!(%error, session_id = %self.render.session_id, "Could not forward ACP session updates");
        }
    }

    async fn start(&mut self, start: Start) {
        let Start {
            message,
            model,
            token,
            done,
        } = start;
        if self.own.is_some() {
            let _ = done.send(Err(error::invalid(
                "A prompt is already active in this session",
            )));
            return;
        }
        if token.is_cancelled() {
            let _ = done.send(Ok(acp::StopReason::Cancelled));
            return;
        }
        if let Err(error) = self.drain().await {
            let _ = done.send(Err(error));
            return;
        }
        match self
            .runtime
            .start_content(
                self.render.session_id.clone(),
                message,
                model.model,
                model.provider,
                model.options,
            )
            .await
        {
            Ok(stream) => {
                self.own = Some(Own {
                    stream,
                    started: false,
                    token,
                    done,
                    forced: None,
                });
                self.own_user = true;
            }
            Err(error) => {
                let _ = done.send(Err(error::runtime(error)));
            }
        }
    }

    fn finish_own(&mut self, result: Result<acp::StopReason>) {
        self.own_user = false;
        if let Some(own) = self.own.take() {
            let _ = own.done.send(own.forced.unwrap_or(result));
        }
    }

    async fn abort_own(&mut self, result: Result<acp::StopReason>) {
        let Some(own) = &mut self.own else {
            return;
        };
        own.forced = Some(result);
        self.permission = None;
        let cancelled = async {
            self.drain().await?;
            if self.own.is_some() {
                self.runtime
                    .cancel_stream(self.render.session_id.clone())
                    .await
                    .map_err(error::runtime)?;
                self.drain().await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        if let Err(error) = cancelled {
            tracing::warn!(%error, session_id = %self.render.session_id, "Could not cancel the ACP prompt's turn");
        }
        self.finish_own(Ok(acp::StopReason::Cancelled));
    }

    async fn drain(&mut self) -> Result<()> {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.event(event).await?,
                Err(broadcast::error::TryRecvError::Lagged(_)) => self.sync().await?,
                Err(broadcast::error::TryRecvError::Empty) => return Ok(()),
                Err(broadcast::error::TryRecvError::Closed) => return Err(closed()),
            }
        }
    }

    async fn event(&mut self, event: RuntimeEvent) -> Result<()> {
        if let Event::ServiceError { error } = &event.event {
            if self.own.is_some() {
                self.abort = Some(error::runtime(error.clone()));
            }
            return Ok(());
        }
        if !self.render.accepts(&event) {
            return Ok(());
        }
        if let Some(own) = &mut self.own
            && !own.started
        {
            own.started = matches!(
                &event.event,
                Event::StreamStart { message_id, .. } if *message_id == own.stream
            );
        }
        let own = self.own.as_ref().is_some_and(|own| own.started);
        match event.event {
            Event::TurnCompleted { .. } => self.end(
                own,
                Ok(acp::StopReason::EndTurn),
                acp::ToolCallStatus::Completed,
            ),
            Event::StreamCancelled { .. } => self.end(
                own,
                Ok(acp::StopReason::Cancelled),
                acp::ToolCallStatus::Failed,
            ),
            Event::StreamError { error, .. } | Event::ContinuationFailed { error, .. } => self.end(
                own,
                Err(error::internal(error)),
                acp::ToolCallStatus::Failed,
            ),
            Event::SessionError { error, .. } => {
                if own {
                    self.abort = Some(error::runtime(error));
                }
                Ok(())
            }
            Event::HistoryUpdated { .. } => self.sync().await,
            Event::ScriptApprovalRequested { script, .. } => self.request(script),
            Event::MessageUpserted { message, .. } => self.upserted(&message).await,
            event @ (Event::ScriptStarted { .. } | Event::ScriptResultReady { .. }) => {
                if let Event::ScriptStarted { call_id, .. }
                | Event::ScriptResultReady { call_id, .. } = &event
                    && self
                        .permission
                        .as_ref()
                        .is_some_and(|permission| &permission.call_id == call_id)
                {
                    self.permission = None;
                }
                self.render.event(&self.runtime, event).await
            }
            event @ (Event::ScriptPrepared { .. }
            | Event::ScriptOutputTail { .. }
            | Event::RequestUsageUpdated { .. }
            | Event::TurnTimingChanged { .. }
            | Event::ConfigLoaded
            | Event::ModelsUpdated
            | Event::ServiceError { .. }
            | Event::StreamStart { .. }
            | Event::ItemStarted { .. }
            | Event::ItemDelta { .. }
            | Event::ReasoningDelta { .. }
            | Event::StreamComplete { .. }
            | Event::ProviderRetryScheduled { .. }
            | Event::StreamRestarted { .. }
            | Event::ContextStateChanged { .. }
            | Event::SessionStateChanged { .. }
            | Event::MessagesRemoved { .. }
            | Event::ProviderAuthUpdated { .. }
            | Event::McpAuthUpdated { .. }) => self.render.event(&self.runtime, event).await,
        }
    }

    fn end(
        &mut self,
        own: bool,
        result: Result<acp::StopReason>,
        retry: acp::ToolCallStatus,
    ) -> Result<()> {
        self.permission = None;
        let settled = self.render.settle_retry(retry);
        if own {
            self.finish_own(result);
        }
        settled
    }

    async fn upserted(&mut self, message: &Message) -> Result<()> {
        if !matches!(message.content, ConversationItem::User { .. }) {
            return Ok(());
        }
        if std::mem::take(&mut self.own_user) {
            self.render.skip_user(message);
            return Ok(());
        }
        self.render.user(&self.runtime, message).await
    }

    async fn sync(&mut self) -> Result<()> {
        self.own_user = false;
        let snapshot = self
            .runtime
            .get_session_snapshot(self.render.session_id.clone())
            .await
            .map_err(error::runtime)?;
        let own_user = self
            .own
            .as_ref()
            .and_then(|own| own_user(&snapshot, &own.stream));
        self.render
            .sync(&self.runtime, &snapshot, own_user.as_ref())
            .await?;
        match (&snapshot.pending_script, &self.permission) {
            (Some(script), Some(permission)) if permission.execution_id == script.execution_id => {}
            (Some(script), _) => self.request(script.clone())?,
            (None, _) => self.permission = None,
        }
        let Some(own) = &mut self.own else {
            return Ok(());
        };
        own.started = true;
        if snapshot.session.is_running {
            return Ok(());
        }
        let (result, retry) = match snapshot.turn_outcome {
            Some(TurnOutcome::Failed { error }) => {
                (Err(error::runtime(error)), acp::ToolCallStatus::Failed)
            }
            Some(TurnOutcome::Cancelled) => {
                (Ok(acp::StopReason::Cancelled), acp::ToolCallStatus::Failed)
            }
            Some(TurnOutcome::Completed) | None => {
                (Ok(acp::StopReason::EndTurn), acp::ToolCallStatus::Completed)
            }
        };
        self.end(true, result, retry)
    }

    fn request(&mut self, script: PendingScriptInfo) -> Result<()> {
        if self
            .permission
            .as_ref()
            .is_some_and(|permission| permission.execution_id == script.execution_id)
        {
            return Ok(());
        }
        self.render
            .announce(script.call_id.clone(), script.source.clone())?;
        let call = acp::ToolCallUpdate::new(
            script.call_id.clone(),
            acp::ToolCallUpdateFields::new()
                .title(format!(
                    "Allow Nushell capabilities: {}",
                    script.capability_additions.join(", ")
                ))
                .kind(acp::ToolKind::Execute)
                .raw_input(content::script_input(script.source)),
        );
        let request = acp::RequestPermissionRequest::new(
            self.render.id.clone(),
            call,
            vec![
                acp::PermissionOption::new(
                    "allow",
                    "Allow once",
                    acp::PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new("deny", "Reject", acp::PermissionOptionKind::RejectOnce),
            ],
        );
        let response = self.render.connection.send_request(request).block_task();
        self.permission = Some(Permission {
            execution_id: script.execution_id,
            call_id: script.call_id,
            response: Some(Box::pin(response)),
        });
        Ok(())
    }

    async fn answer(&mut self, response: Result<acp::RequestPermissionResponse>) -> Result<()> {
        let Some(permission) = &mut self.permission else {
            return Ok(());
        };
        permission.response = None;
        let execution_id = permission.execution_id.clone();
        let session_id = self.render.session_id.clone();
        let decision = match response?.outcome {
            acp::RequestPermissionOutcome::Selected(selected)
                if selected.option_id.0.as_ref() == "allow" =>
            {
                self.runtime.approve_script(session_id, execution_id).await
            }
            acp::RequestPermissionOutcome::Selected(selected)
                if selected.option_id.0.as_ref() == "deny" =>
            {
                self.runtime.deny_script(session_id, execution_id).await
            }
            acp::RequestPermissionOutcome::Cancelled => {
                if let Some(own) = &self.own {
                    own.token.cancel();
                    return Ok(());
                }
                self.runtime.deny_script(session_id, execution_id).await
            }
            acp::RequestPermissionOutcome::Selected(_) | _ => {
                return Err(error::invalid("Unknown permission option"));
            }
        };
        match decision {
            Ok(()) => {
                self.permission = None;
                Ok(())
            }
            Err(error)
                if matches!(
                    error.kind,
                    RuntimeErrorKind::NotFound | RuntimeErrorKind::Conflict
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error::runtime(error)),
        }
    }
}

fn own_user(snapshot: &SessionSnapshot, stream: &str) -> Option<MessageId> {
    let mut cursor = snapshot
        .history
        .get(&MessageId::new(stream))?
        .parent_id
        .clone();
    while let Some(id) = cursor {
        let message = snapshot.history.get(&id)?;
        if matches!(message.content, ConversationItem::User { .. }) {
            return Some(id);
        }
        cursor = message.parent_id.clone();
    }
    None
}

fn closed() -> Error {
    error::internal("Runtime event stream interrupted: closed")
}

async fn cancelled(own: Option<&Own>) {
    match own {
        Some(own) if own.forced.is_none() => own.token.cancelled().await,
        Some(_) | None => std::future::pending().await,
    }
}

async fn answered(permission: Option<&mut Permission>) -> Result<acp::RequestPermissionResponse> {
    match permission.and_then(|permission| permission.response.as_mut()) {
        Some(response) => response.as_mut().await,
        None => std::future::pending().await,
    }
}
