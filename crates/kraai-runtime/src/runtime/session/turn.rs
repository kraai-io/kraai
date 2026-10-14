use color_eyre::eyre::Result;
use kraai_agent::PendingStreamRequest;
use kraai_persistence::{QueuedEntry, QueuedMessage};
use kraai_provider_core::ProviderManager;
use kraai_types::{MessageContent, ModelSelection};

use super::actor::{
    Preparation, PreparationOrigin, Responder, SessionActor, SessionMessage, StreamKind, TurnState,
};
use crate::api::Event;
use crate::{ContinueSessionOutcome, RuntimeError, RuntimeResult, SubmitMessageOutcome};

/// What a preparation child asks the agent to build.
enum PrepareRequest {
    Start {
        message: MessageContent,
        selection: ModelSelection,
    },
    Queue {
        entries: Vec<QueuedEntry>,
        selection: ModelSelection,
    },
    Continuation,
}

impl SessionActor {
    pub(super) async fn send(
        &mut self,
        message: MessageContent,
        selection: ModelSelection,
        response: Responder<SubmitMessageOutcome>,
    ) {
        if self.stopping {
            let _ = response.send(Err(RuntimeError::unavailable("Runtime is stopping")));
            return;
        }
        if let Err(error) = self.validate_submission(&message, &selection).await {
            let _ = response.send(Err(error));
            return;
        }
        let busy = !matches!(self.state, TurnState::Idle)
            || self.runtime.agent_manager.is_turn_active(&self.session_id);
        let queued = match self
            .runtime
            .session_store
            .has_queued_messages(&self.session_id)
            .await
        {
            Ok(queued) => queued,
            Err(error) => {
                let _ = response.send(Err(RuntimeError::from(error)));
                return;
            }
        };
        if busy || queued {
            let position = async {
                self.ensure_lease().await?;
                self.runtime
                    .session_store
                    .enqueue_message(
                        &self.session_id,
                        &QueuedMessage {
                            content: message,
                            selection,
                        },
                    )
                    .await
            }
            .await;
            match position {
                Ok(position) => {
                    let _ = response.send(Ok(SubmitMessageOutcome::Queued { position }));
                    if !busy {
                        self.drain().await;
                    }
                }
                Err(error) => {
                    let _ = response.send(Err(RuntimeError::from(error)));
                }
            }
            return;
        }
        if let Err(error) = self.ensure_lease().await {
            let _ = response.send(Err(RuntimeError::from(error)));
            return;
        }
        self.begin_preparation(
            StreamKind::Initial,
            PreparationOrigin::Send(response),
            PrepareRequest::Start {
                message,
                selection: selection.clone(),
            },
            Some(selection),
            false,
            false,
        );
    }

    pub(in crate::runtime) async fn continue_session(
        &mut self,
        selection: Option<ModelSelection>,
        response: Option<Responder<ContinueSessionOutcome>>,
    ) {
        let idle = matches!(self.state, TurnState::Idle)
            && !self
                .runtime
                .agent_manager
                .session_has_active_stream(&self.session_id)
                .await;
        if self.stopping || !idle {
            if let Some(response) = response {
                let _ = response.send(Ok(ContinueSessionOutcome::NothingToContinue));
            }
            return;
        }
        if let Some(selection) = &selection
            && let Err(error) = self.validate_selection(selection).await
        {
            if let Some(response) = response {
                let _ = response.send(Err(error));
            }
            return;
        }
        self.start_continuation(selection, PreparationOrigin::Continue(response), false)
            .await;
    }

    async fn validate_selection(&self, selection: &ModelSelection) -> RuntimeResult<()> {
        self.runtime
            .agent_manager
            .validate_model_options(
                &selection.provider_id,
                &selection.model_id,
                &selection.options,
                true,
            )
            .await
            .map_err(RuntimeError::from)
    }

    async fn validate_submission(
        &self,
        message: &MessageContent,
        selection: &ModelSelection,
    ) -> RuntimeResult<()> {
        self.validate_selection(selection).await?;
        if message.images().count() > kraai_execution_types::image::MAX_IMAGE_ATTACHMENTS {
            return Err(RuntimeError::invalid_argument("Too many image attachments"));
        }
        if message
            .images()
            .try_fold(0u64, |total, image| total.checked_add(image.byte_length))
            .is_none_or(|total| total > kraai_execution_types::image::MAX_REQUEST_IMAGE_BYTES)
        {
            return Err(RuntimeError::invalid_argument(
                "Image attachments exceed the request byte limit",
            ));
        }
        for image in message.images() {
            self.runtime
                .image_store
                .read(image)
                .await
                .map_err(RuntimeError::internal)?;
        }
        Ok(())
    }

    async fn start_continuation(
        &mut self,
        selection: Option<ModelSelection>,
        origin: PreparationOrigin,
        retried: bool,
    ) {
        let prepared = async {
            let queued = self
                .runtime
                .session_store
                .queued_messages(&self.session_id)
                .await?;
            self.ensure_lease().await?;
            Ok::<_, color_eyre::Report>(queued)
        }
        .await;
        let queued = match prepared {
            Ok(queued) => queued,
            Err(error) => {
                origin.fail(RuntimeError::from(error));
                return;
            }
        };
        let intercepted = !queued.is_empty();
        let request = match selection
            .clone()
            .or_else(|| queued.last().map(|entry| entry.message.selection.clone()))
        {
            Some(selection) => PrepareRequest::Queue {
                entries: queued,
                selection,
            },
            None => PrepareRequest::Continuation,
        };
        self.begin_preparation(
            StreamKind::Continuation,
            origin,
            request,
            selection,
            intercepted,
            retried,
        );
    }

    /// Continues the turn after a script result without a waiting caller.
    pub(in crate::runtime) async fn continue_turn(&mut self) {
        if self.stopping {
            return;
        }
        self.start_continuation(None, PreparationOrigin::Continue(None), false)
            .await;
    }

    /// Starts queued messages when the session has nothing else to do.
    pub(in crate::runtime) async fn drain(&mut self) {
        if self.stopping
            || !matches!(self.state, TurnState::Idle)
            || self.runtime.agent_manager.is_turn_active(&self.session_id)
        {
            return;
        }
        let queued = match self
            .runtime
            .session_store
            .queued_messages(&self.session_id)
            .await
        {
            Ok(queued) => queued,
            Err(error) => {
                self.runtime
                    .send_session_report_error(&self.session_id, error);
                return;
            }
        };
        let Some(selection) = queued.last().map(|entry| entry.message.selection.clone()) else {
            return;
        };
        self.begin_preparation(
            StreamKind::Initial,
            PreparationOrigin::Queue,
            PrepareRequest::Queue {
                entries: queued,
                selection,
            },
            None,
            false,
            false,
        );
    }

    /// Claims the session lease if needed, finishing executions an earlier owner left behind.
    pub(super) async fn ensure_lease(&self) -> Result<()> {
        if self
            .runtime
            .session_store
            .owns_turn(&self.session_id)
            .await?
        {
            return Ok(());
        }
        self.runtime
            .session_store
            .claim_turn(&self.session_id)
            .await?;
        if let Err(error) = self
            .runtime
            .recover_session_executions(&self.session_id)
            .await
        {
            self.runtime.release_turn(&self.session_id).await;
            return Err(error);
        }
        Ok(())
    }

    fn begin_preparation(
        &mut self,
        kind: StreamKind,
        origin: PreparationOrigin,
        request: PrepareRequest,
        selection: Option<ModelSelection>,
        intercepted: bool,
        retried: bool,
    ) {
        let generation = self.generation();
        let agent = self.runtime.agent_manager.clone();
        let session_id = self.session_id.clone();
        let mailbox = self.mailbox();
        self.runtime
            .stream_tasks
            .spawn(&self.session_id, async move {
                let outcome = match request {
                    PrepareRequest::Start { message, selection } => agent
                        .prepare_start_stream(
                            &session_id,
                            message,
                            selection.model_id,
                            selection.provider_id,
                            selection.options,
                        )
                        .await
                        .map(Some),
                    PrepareRequest::Queue { entries, selection } => {
                        agent
                            .prepare_queued_stream(&session_id, entries, selection)
                            .await
                    }
                    PrepareRequest::Continuation => {
                        agent.prepare_continuation_stream(&session_id).await
                    }
                }
                .map(|request| request.map(|request| (agent.cloned_provider_manager(), request)));
                let _ = mailbox.send(SessionMessage::Prepared {
                    generation,
                    outcome,
                });
            });
        self.state = TurnState::Preparing(Preparation {
            generation,
            kind,
            origin,
            selection,
            intercepted,
            retried,
            cancel: None,
        });
    }

    pub(super) async fn prepared(
        &mut self,
        generation: u64,
        outcome: kraai_agent::AgentResult<Option<(ProviderManager, PendingStreamRequest)>>,
    ) {
        let preparation = match std::mem::replace(&mut self.state, TurnState::Idle) {
            TurnState::Preparing(preparation) if preparation.generation == generation => {
                preparation
            }
            state => {
                self.state = state;
                return;
            }
        };
        let Preparation {
            kind,
            origin,
            selection,
            intercepted,
            retried,
            cancel,
            ..
        } = preparation;
        match outcome {
            Ok(Some((providers, request))) if !self.stopping => {
                let message_id = request.message_id.to_string();
                self.start_stream(kind, providers, request);
                origin.started(message_id);
                if let Some(cancel) = cancel
                    && let TurnState::Streaming(job) =
                        std::mem::replace(&mut self.state, TurnState::Idle)
                {
                    self.cancel_stream_job(job, cancel).await;
                }
            }
            Ok(Some(_)) => {
                origin.fail(RuntimeError::unavailable("Runtime is stopping"));
                if let Some(cancel) = cancel {
                    cancel.answer(Ok(true));
                }
            }
            Ok(None) => {
                match &origin {
                    PreparationOrigin::Send(_) => self.finish_turn().await,
                    PreparationOrigin::Continue(_) => self.finish_turn().await,
                    PreparationOrigin::Queue => {}
                }
                origin.nothing_started();
                if let Some(cancel) = cancel {
                    let discarded = cancel.discarded;
                    cancel.answer(Ok(discarded));
                }
            }
            Err(error) => {
                if let Some(cancel) = cancel {
                    self.finish_turn().await;
                    origin.fail(RuntimeError::from(error));
                    cancel.answer(Ok(true));
                    return;
                }
                match origin {
                    PreparationOrigin::Continue(response)
                        if intercepted && !retried && !self.stopping =>
                    {
                        self.start_continuation(
                            selection,
                            PreparationOrigin::Continue(response),
                            true,
                        )
                        .await;
                    }
                    PreparationOrigin::Continue(response) => {
                        self.finish_turn().await;
                        if !intercepted {
                            self.drain().await;
                        }
                        match response {
                            Some(response) => {
                                let _ = response.send(Err(RuntimeError::from(error)));
                            }
                            None => self.runtime.send_event(Event::ContinuationFailed {
                                session_id: self.session_id.clone(),
                                error: error.to_string(),
                            }),
                        }
                    }
                    PreparationOrigin::Send(response) => {
                        self.finish_turn().await;
                        let _ = response.send(Err(RuntimeError::from(error)));
                    }
                    PreparationOrigin::Queue => {
                        self.finish_turn().await;
                        self.runtime
                            .send_session_report_error(&self.session_id, error);
                    }
                }
            }
        }
    }
}

impl PreparationOrigin {
    fn started(self, message_id: String) {
        match self {
            Self::Send(response) => {
                let _ = response.send(Ok(SubmitMessageOutcome::Started { message_id }));
            }
            Self::Continue(Some(response)) => {
                let _ = response.send(Ok(ContinueSessionOutcome::Started));
            }
            Self::Continue(None) | Self::Queue => {}
        }
    }

    fn nothing_started(self) {
        match self {
            Self::Send(response) => {
                let _ = response.send(Err(RuntimeError::internal(
                    "Failed to prepare a new message stream",
                )));
            }
            Self::Continue(Some(response)) => {
                let _ = response.send(Ok(ContinueSessionOutcome::NothingToContinue));
            }
            Self::Continue(None) | Self::Queue => {}
        }
    }

    fn fail(self, error: RuntimeError) {
        match self {
            Self::Send(response) => {
                let _ = response.send(Err(error));
            }
            Self::Continue(Some(response)) => {
                let _ = response.send(Err(error));
            }
            Self::Continue(None) | Self::Queue => {}
        }
    }
}

impl super::actor::CancelRequest {
    pub(super) fn new(discarded: bool, response: Option<Responder<bool>>) -> Self {
        Self {
            discarded,
            waiters: response.into_iter().collect(),
        }
    }

    /// Joins another cancellation of the same preparation.
    pub(super) fn merge(&mut self, discarded: bool, response: Option<Responder<bool>>) {
        self.discarded |= discarded;
        self.waiters.extend(response);
    }

    pub(super) fn answer(self, result: RuntimeResult<bool>) {
        for waiter in self.waiters {
            let _ = waiter.send(result.clone());
        }
    }
}
