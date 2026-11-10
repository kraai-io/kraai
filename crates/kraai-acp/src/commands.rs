use agent_client_protocol::{Result, schema::v1 as acp};
use kraai_runtime::RuntimeHandle;

use crate::{content, error};

/// Whether the prompt is the `/undo` command.
pub(crate) fn is_undo(prompt: &[acp::ContentBlock]) -> Result<bool> {
    let Some(acp::ContentBlock::Text(text)) = prompt.first() else {
        return Ok(false);
    };
    let mut words = text.text.split_whitespace();
    if words.next() != Some("/undo") {
        return Ok(false);
    }
    if words.next().is_some() {
        return Err(error::invalid("Usage: /undo"));
    }
    Ok(true)
}

pub(crate) async fn advertise(
    connection: &crate::transport::Connection,
    id: acp::SessionId,
) -> Result<()> {
    connection
        .send_notification_wait(acp::SessionNotification::new(
            id,
            acp::SessionUpdate::AvailableCommandsUpdate(acp::AvailableCommandsUpdate::new(vec![
                acp::AvailableCommand::new(
                    "undo",
                    "Remove the last user turn from conversation context; keep file changes",
                ),
            ])),
        ))
        .await
}

pub(crate) async fn undo(
    runtime: &RuntimeHandle,
    id: &kraai_types::SessionId,
    connection: &crate::transport::Connection,
) -> Result<()> {
    let restored = runtime
        .undo_last_user_message(id.clone())
        .await
        .map_err(error::runtime)?;
    let text = if restored.is_some() {
        "Undid the last user message and its replies in Kraai's context. File changes were not reverted. Your client may still display the old messages."
    } else {
        "No user message to undo."
    };
    connection.send_notification(acp::SessionNotification::new(
        id.to_string(),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(content::text(text))),
    ))
}
