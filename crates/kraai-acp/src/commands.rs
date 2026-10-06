use agent_client_protocol::{Result, schema::v1 as acp};
use kraai_runtime::RuntimeHandle;

use crate::{content, error};

pub(crate) enum Command {
    Continue,
    Undo,
}

pub(crate) fn parse(prompt: &[acp::ContentBlock]) -> Result<Option<Command>> {
    let [acp::ContentBlock::Text(text)] = prompt else {
        return Ok(None);
    };
    let mut words = text.text.split_whitespace();
    let (command, usage) = match words.next() {
        Some("/continue") => (Command::Continue, "Usage: /continue"),
        Some("/undo") => (Command::Undo, "Usage: /undo"),
        _ => return Ok(None),
    };
    if words.next().is_some() {
        return Err(error::invalid(usage));
    }
    Ok(Some(command))
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
                    "continue",
                    "Resume the conversation using the selected model",
                ),
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
    id: &acp::SessionId,
    connection: &crate::transport::Connection,
) -> Result<()> {
    let restored = runtime
        .undo_last_user_message(id.to_string())
        .await
        .map_err(error::runtime)?;
    let text = if restored.is_some() {
        "Undid the last user message and its replies in Kraai's context. File changes were not reverted. Your client may still display the old messages."
    } else {
        "No user message to undo."
    };
    connection.send_notification(acp::SessionNotification::new(
        id.clone(),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(content::text(text))),
    ))
}
