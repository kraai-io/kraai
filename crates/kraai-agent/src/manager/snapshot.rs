use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use color_eyre::eyre::Result;
use kraai_persistence::{HistoryCursor, MessageStore, SessionMeta};
use kraai_types::{AgentProfilesState, ChatRole, Message, MessageId, MessageStatus, SessionId};

use super::{AgentManager, AgentResult, SessionContextUsage};

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
    persisted_history: Option<BTreeMap<MessageId, Message>>,
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
        session_id: &SessionId,
    ) -> AgentResult<SessionSnapshotReader> {
        let persisted = self.message_store.read_conversation(session_id).await?;
        let (session, persisted_history, requests) = match persisted {
            Some(snapshot) => (snapshot.session, Some(snapshot.history), snapshot.requests),
            None => (
                self.require_session(session_id).await?,
                None,
                self.usage_store.load(session_id).await?,
            ),
        };
        let in_flight = self.capture_in_flight_messages(Some(session_id)).await;
        Ok(SessionSnapshotReader {
            session,
            profile_locked: self.is_profile_locked(session_id),
            streaming: !in_flight.is_empty(),
            messages: self.message_store.clone(),
            storage_root: self.storage_root.clone(),
            in_flight,
            requests,
            persisted_history,
        })
    }

    pub(super) async fn capture_in_flight_messages(
        &self,
        session_id: Option<&SessionId>,
    ) -> HashMap<MessageId, Message> {
        let streaming = self.streaming_messages.read().await;
        let in_flight: HashMap<_, _> = streaming
            .iter()
            .filter(|(_, state)| session_id.is_none_or(|id| state.session_id == *id))
            .map(|(id, state)| (id.clone(), state.message.clone()))
            .collect();
        drop(streaming);
        in_flight
    }

    pub(super) async fn history_context(&self, from: &MessageId) -> AgentResult<Vec<Message>> {
        let in_flight = self.capture_in_flight_messages(None).await;
        Ok(
            HistoryCursor::new(self.message_store.as_ref(), Some(from.clone()))
                .with_overlay(&in_flight)
                .read_to_root()
                .await?,
        )
    }

    /// Reads up to `limit` messages ending at `from`, newest first, and where the next older
    /// page starts.
    pub async fn history_page(
        &self,
        from: &MessageId,
        limit: usize,
    ) -> AgentResult<(Vec<Message>, Option<MessageId>)> {
        let in_flight = self.capture_in_flight_messages(None).await;
        Ok(
            HistoryCursor::new(self.message_store.as_ref(), Some(from.clone()))
                .with_overlay(&in_flight)
                .read_page(limit)
                .await?,
        )
    }

    pub async fn get_session_context_usage(
        &self,
        session_id: &SessionId,
    ) -> AgentResult<Option<SessionContextUsage>> {
        let Some(tip_id) = self.get_tip(session_id).await? else {
            return Ok(None);
        };
        let in_flight = self.capture_in_flight_messages(None).await;
        let mut history =
            HistoryCursor::new(self.message_store.as_ref(), Some(tip_id)).with_overlay(&in_flight);
        while let Some(page) = history.next_page().await? {
            if let Some(usage) = page.iter().find_map(message_context_usage) {
                return Ok(Some(usage));
            }
        }
        Ok(None)
    }
}

impl SessionSnapshotReader {
    /// Performs history and profile I/O without holding the runtime or manager locks.
    pub async fn load(self) -> Result<SessionSnapshotData> {
        let mut history = self.persisted_history.unwrap_or_default();
        if history.is_empty() && self.session.tip_id.is_some() {
            history = kraai_persistence::history_by_id(
                HistoryCursor::new(self.messages.as_ref(), self.session.tip_id.clone())
                    .with_overlay(&self.in_flight)
                    .read_to_root()
                    .await?,
            );
        }
        history.extend(self.in_flight);
        let mut context_usage = None;
        let mut cursor = self.session.tip_id.clone();
        while let Some(message) = cursor.as_ref().and_then(|id| history.get(id)) {
            if let Some(usage) = message_context_usage(message) {
                context_usage = Some(usage);
                break;
            }
            cursor = message.parent_id.clone();
        }

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

pub(super) fn message_context_usage(message: &Message) -> Option<SessionContextUsage> {
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
