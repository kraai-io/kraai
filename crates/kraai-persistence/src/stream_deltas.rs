use color_eyre::eyre::{Result, eyre};
use kraai_types::{
    AssistantItem, AssistantPhase, ConversationItem, Message, MessageId, MessageStatus, TokenUsage,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::database::{LeaseTokens, assert_owner, bump_revision, read_record, record_session};

/// An incremental change to a streaming assistant message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamDelta {
    /// Appends text to the item at `index`, creating it when `index` is the item count.
    Text {
        index: usize,
        phase: AssistantPhase,
        text: String,
    },
    Item {
        item: AssistantItem,
    },
    Usage {
        usage: TokenUsage,
    },
    /// Discards the partial response before the request is issued again.
    Reset,
}

impl StreamDelta {
    pub fn apply(&self, message: &mut Message) -> Result<()> {
        if let Self::Usage { usage } = self {
            if let Some(generation) = message.generation.as_mut() {
                generation.usage = Some(usage.clone());
            }
            return Ok(());
        }
        let ConversationItem::Assistant { items } = &mut message.content else {
            return Err(eyre!(
                "Stream delta targets non-assistant message {}",
                message.id
            ));
        };
        match self {
            Self::Reset => items.clear(),
            Self::Text { index, phase, text } if *index == items.len() => {
                items.push(AssistantItem::Text {
                    phase: *phase,
                    text: text.clone(),
                });
            }
            Self::Text { index, phase, text } => match items.get_mut(*index) {
                Some(AssistantItem::Text {
                    phase: stored,
                    text: stored_text,
                }) if stored == phase => stored_text.push_str(text),
                _ => {
                    return Err(eyre!(
                        "Stream delta targets missing text item {index} in message {}",
                        message.id
                    ));
                }
            },
            Self::Item { item } => items.push(item.clone()),
            Self::Usage { .. } => {}
        }
        Ok(())
    }

    /// Merges `next` into this delta when both append to the same text item.
    pub fn coalesce(&mut self, next: Self) -> Option<Self> {
        match (self, next) {
            (
                Self::Text { index, phase, text },
                Self::Text {
                    index: next_index,
                    phase: next_phase,
                    text: next_text,
                },
            ) if *index == next_index && *phase == next_phase => {
                text.push_str(&next_text);
                None
            }
            (_, next) => Some(next),
        }
    }
}

pub(crate) fn read_message(connection: &Connection, id: &str) -> Result<Option<Message>> {
    let Some(mut message) = read_record::<Message>(connection, "message", id)? else {
        return Ok(None);
    };
    if matches!(message.status, MessageStatus::Streaming { .. }) {
        let mut statement = connection.prepare_cached(
            "SELECT data FROM stream_deltas WHERE message_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
        for row in rows {
            serde_json::from_str::<StreamDelta>(&row?)?.apply(&mut message)?;
        }
    }
    Ok(Some(message))
}

pub(crate) fn append_stream_deltas(
    connection: &Connection,
    leases: &LeaseTokens,
    message_id: &MessageId,
    first_sequence: u64,
    deltas: &[StreamDelta],
) -> Result<()> {
    let session = record_session(connection, "message", message_id.as_str())?
        .ok_or_else(|| eyre!("Cannot stream into unlinked message {message_id}"))?;
    assert_owner(connection, leases, &session)?;
    let mut statement = connection.prepare_cached(
        "INSERT OR IGNORE INTO stream_deltas(message_id, sequence, data) VALUES (?1, ?2, ?3)",
    )?;
    for (sequence, delta) in (first_sequence..).zip(deltas) {
        statement.execute(params![
            message_id.as_str(),
            i64::try_from(sequence)?,
            serde_json::to_string(delta)?
        ])?;
    }
    bump_revision(connection, &session)
}

pub(crate) fn clear_stream_deltas(connection: &Connection, message_id: &str) -> Result<()> {
    connection.execute(
        "DELETE FROM stream_deltas WHERE message_id = ?1",
        [message_id],
    )?;
    Ok(())
}
