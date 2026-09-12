use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{MessageStore, SessionMeta};
use kraai_types::{AgentProfilesState, Message, MessageId};

use super::{AgentManager, SessionContextUsage};

/// Captures mutable session state before loading its persisted history.
/// Completed messages are immutable in the manager. In-flight messages must be
/// copied here because completion or cancellation can replace or delete them.
pub struct SessionSnapshotReader {
    pub session: SessionMeta,
    pub profile_locked: bool,
    pub streaming: bool,
    messages: Arc<dyn MessageStore>,
    in_flight: HashMap<MessageId, Message>,
    requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
}

pub struct SessionSnapshotData {
    pub requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
    pub history: BTreeMap<MessageId, Message>,
    pub context_usage: Option<SessionContextUsage>,
    pub profiles: AgentProfilesState,
}

impl AgentManager {
    pub async fn capture_session_snapshot(
        &self,
        session_id: &str,
    ) -> Result<SessionSnapshotReader> {
        let session = self.require_session(session_id).await?;
        let streaming = self.streaming_messages.read().await;
        let in_flight: HashMap<_, _> = streaming
            .iter()
            .filter(|(_, state)| state.session_id == session_id)
            .map(|(id, state)| (id.clone(), state.message.clone()))
            .collect();
        drop(streaming);
        Ok(SessionSnapshotReader {
            session,
            profile_locked: self.is_profile_locked(session_id),
            streaming: !in_flight.is_empty(),
            messages: self.message_store.clone(),
            in_flight,
            requests: self.usage_store.load(session_id).await?,
        })
    }
}

impl SessionSnapshotReader {
    /// Performs history and profile I/O without holding the runtime or manager locks.
    pub async fn load(&self) -> Result<SessionSnapshotData> {
        let context = load_history(
            self.messages.as_ref(),
            self.session.tip_id.clone(),
            &self.in_flight,
        )
        .await?;
        let context_usage = super::streaming::context_usage(&context);
        let mut history: BTreeMap<_, _> = context
            .into_iter()
            .map(|message| (message.id.clone(), message))
            .collect();
        history.extend(
            self.in_flight
                .iter()
                .map(|(id, message)| (id.clone(), message.clone())),
        );

        let mut requests = self.requests.clone();
        for message in history.values() {
            if let Some(generation) = &message.generation {
                requests
                    .entry(message.id.clone())
                    .or_insert_with(|| kraai_types::RequestUsage {
                        message_id: message.id.clone(),
                        provider_id: generation.provider_id.clone(),
                        model_id: generation.model_id.clone(),
                        started_at: 0,
                        subscription: false,
                        usage: generation.usage.clone(),
                    });
            }
        }

        let workspace = self.session.workspace_dir.clone();
        let resolved = tokio::task::spawn_blocking(move || {
            crate::profiles::resolve_profiles(&workspace, &crate::profiles::available_command_ids())
        })
        .await?;
        let profiles = AgentProfilesState {
            profiles: resolved
                .profiles
                .iter()
                .map(crate::profiles::AgentProfile::summary)
                .collect(),
            warnings: resolved.warnings,
            selected_profile_id: self.session.selected_profile_id.clone(),
            profile_locked: self.profile_locked,
        };
        Ok(SessionSnapshotData {
            requests,
            history,
            context_usage,
            profiles,
        })
    }
}

pub(super) async fn load_history(
    store: &dyn MessageStore,
    mut current: Option<MessageId>,
    in_flight: &HashMap<MessageId, Message>,
) -> Result<Vec<Message>> {
    let mut context = Vec::new();
    while let Some(id) = current {
        let message = match in_flight.get(&id) {
            Some(message) => message.clone(),
            None => store
                .get(&id)
                .await?
                .ok_or_else(|| eyre!("Message {id} disappeared while reading session history"))?,
        };
        current = message.parent_id.clone();
        context.push(message);
    }
    context.reverse();
    Ok(context)
}
