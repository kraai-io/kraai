use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{MessageStore, SessionMeta};
use kraai_types::{AgentProfilesState, ChatRole, Message, MessageId, MessageStatus};

use super::{AgentManager, SessionContextUsage};

/// Captures mutable session state before loading its persisted history.
/// Completed messages are immutable in the manager. In-flight messages must be
/// copied here because completion or cancellation can replace or delete them.
pub struct SessionSnapshotReader {
    session: SessionMeta,
    profile_locked: bool,
    streaming: bool,
    messages: Arc<dyn MessageStore>,
    storage_root: std::path::PathBuf,
    in_flight: HashMap<MessageId, Message>,
    requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
}

pub struct SessionSnapshotData {
    pub session: SessionMeta,
    pub profile_locked: bool,
    pub streaming: bool,
    pub requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
    pub history: BTreeMap<MessageId, Message>,
    pub context_usage: Option<SessionContextUsage>,
    pub profiles: AgentProfilesState,
}

impl AgentManager {
    pub fn request_usage_store(&self) -> Arc<dyn kraai_persistence::RequestUsageStore> {
        self.usage_store.clone()
    }

    pub async fn capture_session_snapshot(
        &self,
        session_id: &str,
    ) -> Result<SessionSnapshotReader> {
        let session = self.require_session(session_id).await?;
        let in_flight = self.capture_in_flight_messages(Some(session_id)).await;
        Ok(SessionSnapshotReader {
            session,
            profile_locked: self.is_profile_locked(session_id),
            streaming: !in_flight.is_empty(),
            messages: self.message_store.clone(),
            storage_root: self.storage_root.clone(),
            in_flight,
            requests: self.usage_store.load(session_id).await?,
        })
    }

    async fn capture_in_flight_messages(
        &self,
        session_id: Option<&str>,
    ) -> HashMap<MessageId, Message> {
        let streaming = self.streaming_messages.read().await;
        let in_flight: HashMap<_, _> = streaming
            .iter()
            .filter(|(_, state)| session_id.is_none_or(|id| state.session_id == id))
            .map(|(id, state)| (id.clone(), state.message.clone()))
            .collect();
        drop(streaming);
        in_flight
    }

    pub(super) async fn visit_history_context(
        &self,
        from: &MessageId,
        visit: impl FnMut(Cow<'_, Message>) + Send,
    ) -> Result<()> {
        let in_flight = self.capture_in_flight_messages(None).await;
        visit_history(
            self.message_store.as_ref(),
            Some(from.clone()),
            &in_flight,
            visit,
        )
        .await
    }

    pub async fn get_session_context_usage(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionContextUsage>> {
        let Some(tip_id) = self.get_tip(session_id).await? else {
            return Ok(None);
        };

        let mut usage = None;
        self.visit_history_context(&tip_id, |message| {
            if usage.is_none() {
                usage = message_context_usage(&message);
            }
        })
        .await?;
        Ok(usage)
    }
}

impl SessionSnapshotReader {
    /// Performs history and profile I/O without holding the runtime or manager locks.
    pub async fn load(self) -> Result<SessionSnapshotData> {
        let mut context_usage = None;
        let mut history = BTreeMap::new();
        visit_history(
            self.messages.as_ref(),
            self.session.tip_id.clone(),
            &self.in_flight,
            |message| {
                if context_usage.is_none() {
                    context_usage = message_context_usage(&message);
                }
                if let Cow::Owned(message) = message {
                    history.entry(message.id.clone()).or_insert(message);
                }
            },
        )
        .await?;
        history.extend(self.in_flight);

        let mut requests = self.requests;
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
                        unpriced_attempts: 0,
                        usage: generation.usage.clone(),
                    });
            }
        }

        let workspace = self.session.workspace_dir.clone();
        let storage_root = self.storage_root;
        let resolved = tokio::task::spawn_blocking(move || {
            crate::profiles::resolve_profiles(
                &workspace,
                &storage_root,
                &crate::profiles::available_command_ids(),
            )
        })
        .await?;
        let profiles = AgentProfilesState {
            profiles: resolved
                .profiles
                .into_iter()
                .map(crate::profiles::AgentProfile::into_summary)
                .collect(),
            warnings: resolved.warnings,
            selected_profile_id: self.session.selected_profile_id.clone(),
            profile_locked: self.profile_locked,
        };
        Ok(SessionSnapshotData {
            session: self.session,
            profile_locked: self.profile_locked,
            streaming: self.streaming,
            requests,
            history,
            context_usage,
            profiles,
        })
    }
}

async fn visit_history(
    store: &dyn MessageStore,
    mut current: Option<MessageId>,
    in_flight: &HashMap<MessageId, Message>,
    mut visit: impl FnMut(Cow<'_, Message>) + Send,
) -> Result<()> {
    let mut visited = HashSet::new();
    while let Some(id) = current {
        if !visited.insert(id.clone()) {
            return Err(eyre!(
                "Corrupt message parent graph: cycle repeats message {id}"
            ));
        }
        let message =
            match in_flight.get(&id) {
                Some(message) => Cow::Borrowed(message),
                None => Cow::Owned(store.get(&id).await?.ok_or_else(|| {
                    eyre!("Message {id} disappeared while reading session history")
                })?),
            };
        current = message.parent_id.clone();
        visit(message);
    }
    Ok(())
}

fn message_context_usage(message: &Message) -> Option<SessionContextUsage> {
    (message.role() == ChatRole::Assistant && message.status == MessageStatus::Complete)
        .then_some(message.generation.as_ref())
        .flatten()
        .and_then(|generation| {
            generation.usage.as_ref().map(|usage| SessionContextUsage {
                provider_id: generation.provider_id.clone(),
                model_id: generation.model_id.clone(),
                max_context: generation.max_context,
                usage: usage.clone(),
            })
        })
}
