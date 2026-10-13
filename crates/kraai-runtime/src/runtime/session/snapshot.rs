use color_eyre::eyre::Result;
use kraai_agent::SessionSnapshotReader;
use std::collections::BTreeMap;

use kraai_types::{Message, MessageId, MessageStatus, ScriptExecutionPhase};

use super::super::client_view::client_message;

use super::actor::{Responder, SessionActor, TurnState};
use crate::api::{
    PendingScriptInfo, RuntimeError, Session, SessionActivity, SessionContextUsage, SessionSnapshot,
};

/// The parts of a snapshot captured inside the actor, coherent with the session's events.
struct CapturedSnapshot {
    reader: SessionSnapshotReader,
    foreign_owner: bool,
    is_running: bool,
    pending_script: Option<PendingScriptInfo>,
    executing_script: bool,
    active_stream: bool,
    queued_messages: usize,
    event_sequence: u64,
    turn_timer: crate::TurnTimer,
}

impl SessionActor {
    pub(super) async fn snapshot(&mut self, response: Responder<SessionSnapshot>) {
        let captured = match self.capture_snapshot().await {
            Ok(captured) => captured,
            Err(error) => {
                let _ = response.send(Err(RuntimeError::from(error)));
                return;
            }
        };
        let execution_store = self.runtime.execution_store.clone();
        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            let result = finish_snapshot(captured, execution_store, &session_id).await;
            let _ = response.send(result.map_err(RuntimeError::from));
        });
    }

    async fn capture_snapshot(&self) -> Result<CapturedSnapshot> {
        let store = &self.runtime.session_store;
        let observation = store.observe(&self.session_id).await?;
        if let Some(observation) = observation {
            self.runtime
                .observed_sessions
                .lock()
                .await
                .insert(self.session_id.clone(), observation);
        }
        let foreign_owner = observation.is_some_and(|observation| observation.lease_active)
            && !store.owns_turn(&self.session_id).await?;
        let barrier = self.runtime.sessions.barrier(&self.session_id);
        let guard = barrier.write().await;
        let queued_messages = store.queued_messages(&self.session_id).await?.len();
        let reader = self
            .runtime
            .agent_manager
            .capture_session_snapshot(&self.session_id)
            .await?;
        let is_running =
            self.runtime.agent_manager.is_turn_active(&self.session_id) || foreign_owner;
        let (event_sequence, turn_timer) = self.runtime.event_tx.timer_snapshot(&self.session_id);
        drop(guard);
        Ok(CapturedSnapshot {
            reader,
            foreign_owner,
            is_running,
            pending_script: self.pending_script_info(),
            executing_script: matches!(self.state, TurnState::RunningScript(_)),
            active_stream: matches!(self.state, TurnState::Streaming(_)),
            queued_messages,
            event_sequence,
            turn_timer,
        })
    }
}

/// How many of the latest messages a snapshot carries; clients page through older ones.
pub(crate) const SNAPSHOT_HISTORY_LIMIT: usize = 200;

/// The latest messages on the chain ending at `tip`, and where the older ones continue.
fn latest_history(
    mut history: BTreeMap<MessageId, Message>,
    tip: Option<&MessageId>,
) -> (BTreeMap<MessageId, Message>, Option<MessageId>) {
    let mut latest = BTreeMap::new();
    let mut cursor = tip.cloned();
    while latest.len() < SNAPSHOT_HISTORY_LIMIT
        && let Some(message) = cursor.as_ref().and_then(|id| history.remove(id))
    {
        cursor = message.parent_id.clone();
        latest.insert(message.id.clone(), client_message(message));
    }
    (latest, cursor)
}

async fn finish_snapshot(
    captured: CapturedSnapshot,
    execution_store: std::sync::Arc<dyn kraai_persistence::ScriptExecutionStore>,
    session_id: &kraai_types::SessionId,
) -> Result<SessionSnapshot> {
    let CapturedSnapshot {
        reader,
        foreign_owner,
        is_running,
        pending_script,
        executing_script,
        active_stream,
        queued_messages,
        event_sequence,
        turn_timer,
    } = captured;
    let mut data = reader.load().await?;
    data.profiles.profile_locked |= foreign_owner;
    let context_usage = data.context_usage.map(|usage| SessionContextUsage {
        provider_id: usage.provider_id.to_string(),
        model_id: usage.model_id.to_string(),
        max_context: usage.max_context,
        usage: usage.usage,
    });
    let foreign_records = if foreign_owner {
        execution_store.list_for_session(session_id).await?
    } else {
        Vec::new()
    };
    let foreign_approval = foreign_records
        .iter()
        .any(|record| record.phase == ScriptExecutionPhase::AwaitingApproval);
    let foreign_script = foreign_records
        .iter()
        .any(|record| record.phase == ScriptExecutionPhase::Running);
    let streaming = active_stream
        || data.streaming
        || (foreign_owner
            && data
                .history
                .values()
                .any(|message| matches!(message.status, MessageStatus::Streaming { .. })));
    let activity = if pending_script.is_some() || foreign_approval {
        SessionActivity::AwaitingApproval
    } else if executing_script || foreign_script {
        SessionActivity::ExecutingScript
    } else if streaming {
        SessionActivity::Streaming
    } else {
        SessionActivity::Idle
    };
    let session = Session {
        profile_locked: data.profile_locked || foreign_owner,
        waiting_for_approval: pending_script.is_some() || foreign_approval,
        is_streaming: streaming,
        is_running,
        ..Session::from_session_meta(data.session)
    };
    let tip = session.tip_id.clone().map(MessageId::new);
    let (history, older_history) = latest_history(data.history, tip.as_ref());
    Ok(SessionSnapshot {
        requests: data.requests,
        turn_timer,
        event_sequence,
        session,
        history,
        older_history,
        context_usage,
        pending_script,
        profiles: data.profiles,
        activity,
        queued_messages,
    })
}
