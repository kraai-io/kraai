use color_eyre::eyre::Result;

use super::super::script_execution::{CompletedScriptExecution, PendingScriptApproval};
use super::actor::{ActivityKind, SessionActor, TurnState};

impl SessionActor {
    pub(in crate::runtime) fn activity(&self) -> ActivityKind {
        self.state.kind()
    }

    /// Removes a pending approval as if the runtime had stopped before deciding it.
    pub(in crate::runtime) fn take_pending_approval(&mut self) -> Option<PendingScriptApproval> {
        match std::mem::replace(&mut self.state, TurnState::Idle) {
            TurnState::AwaitingApproval(pending) => Some(*pending),
            state => {
                self.state = state;
                None
            }
        }
    }

    pub(in crate::runtime) async fn finish_script(
        &mut self,
        completed: CompletedScriptExecution,
    ) -> Result<()> {
        self.finalize_script_turn(completed).await
    }
}

impl SessionActor {
    /// Marks the session as preparing a turn without starting one.
    pub(in crate::runtime) fn begin_test_preparation(&mut self) {
        let generation = self.generation();
        self.state = TurnState::Preparing(super::actor::Preparation {
            generation,
            kind: super::actor::StreamKind::Initial,
            origin: super::actor::PreparationOrigin::Queue,
            intercepted: false,
            retried: false,
            cancel: None,
        });
    }

    pub(in crate::runtime) fn end_test_preparation(&mut self) {
        if let TurnState::Preparing(preparation) =
            std::mem::replace(&mut self.state, TurnState::Idle)
            && let Some(cancel) = preparation.cancel
        {
            let discarded = cancel.discarded;
            cancel.answer(Ok(discarded));
        }
    }

    pub(in crate::runtime) fn start_test_stream(
        &mut self,
        providers: kraai_provider_core::ProviderManager,
        request: kraai_agent::PendingStreamRequest,
    ) {
        self.start_stream(super::actor::StreamKind::Initial, providers, request);
    }

    pub(in crate::runtime) fn test_mailbox(
        &self,
    ) -> tokio::sync::mpsc::UnboundedSender<super::SessionMessage> {
        self.mailbox()
    }

    /// Captures a snapshot at this point in the actor's message order.
    pub(in crate::runtime) async fn request_snapshot(
        &mut self,
    ) -> tokio::sync::oneshot::Receiver<crate::RuntimeResult<crate::SessionSnapshot>> {
        let (response, snapshot) = tokio::sync::oneshot::channel();
        self.snapshot(response).await;
        snapshot
    }

    /// Marks the session as running a script whose result the test reports.
    pub(in crate::runtime) fn begin_test_script(
        &mut self,
    ) -> (u64, tokio_util::sync::CancellationToken) {
        let generation = self.generation();
        let cancel = tokio_util::sync::CancellationToken::new();
        self.state = TurnState::RunningScript(super::actor::ScriptJob {
            generation,
            cancel: cancel.clone(),
            cancel_waiters: Vec::new(),
        });
        (generation, cancel)
    }
}
