use std::collections::HashSet;

use agent_client_protocol::{Client, ConnectionTo, Result, schema::v1 as acp};
use kraai_runtime::{RuntimeHandle, SessionSnapshot};
use kraai_types::{AssistantItem, ConversationItem};

use crate::{content, error};

pub(crate) async fn replay(
    runtime: &RuntimeHandle,
    snapshot: &SessionSnapshot,
    connection: &ConnectionTo<Client>,
) -> Result<()> {
    let mut tip = snapshot
        .session
        .tip_id
        .as_ref()
        .map(|id| kraai_types::MessageId::new(id.clone()));
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    while let Some(id) = tip {
        if !seen.insert(id.clone()) {
            return Err(error::internal("Conversation history contains a cycle"));
        }
        let message = snapshot
            .history
            .get(&id)
            .ok_or_else(|| error::internal("Conversation history is incomplete"))?;
        messages.push(message);
        tip = message.parent_id.clone();
    }
    let session_id = acp::SessionId::new(snapshot.session.id.clone());
    for message in messages.into_iter().rev() {
        for update in updates(runtime, message).await? {
            connection
                .send_notification(acp::SessionNotification::new(session_id.clone(), update))?;
        }
    }
    Ok(())
}

async fn updates(
    runtime: &RuntimeHandle,
    message: &kraai_types::Message,
) -> Result<Vec<acp::SessionUpdate>> {
    let chunk = |block| {
        acp::ContentChunk::new(block).message_id(acp::MessageId::new(message.id.to_string()))
    };
    Ok(match &message.content {
        ConversationItem::User { content } => content::blocks(runtime, content)
            .await?
            .into_iter()
            .map(|block| acp::SessionUpdate::UserMessageChunk(chunk(block)))
            .collect(),
        ConversationItem::Assistant { items } => assistant_updates(&message.id, items),
        ConversationItem::ScriptResult { call_id, output } => {
            vec![content::tool_result(runtime, call_id.to_string(), output).await?]
        }
        _ => Vec::new(),
    })
}

fn assistant_updates(
    id: &kraai_types::MessageId,
    items: &[AssistantItem],
) -> Vec<acp::SessionUpdate> {
    let mut has_prior_item = false;
    items
        .iter()
        .filter_map(|item| {
            let update = match item {
                AssistantItem::Text { text, .. } => {
                    let text = if has_prior_item && !text.is_empty() {
                        format!("\n\n{text}")
                    } else {
                        text.clone()
                    };
                    acp::SessionUpdate::AgentMessageChunk(
                        acp::ContentChunk::new(content::text(text))
                            .message_id(acp::MessageId::new(id.to_string())),
                    )
                }
                AssistantItem::ScriptCall { call_id, input, .. } => acp::SessionUpdate::ToolCall(
                    acp::ToolCall::new(call_id.to_string(), "Run Nushell script")
                        .kind(acp::ToolKind::Execute)
                        .status(acp::ToolCallStatus::Pending)
                        .raw_input(serde_json::Value::String(input.clone())),
                ),
                AssistantItem::Reasoning { .. } => return None,
            };
            has_prior_item = true;
            Some(update)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kraai_types::{AssistantPhase, MessageId, ProviderId};

    #[test]
    fn replay_preserves_spacing_between_assistant_text_items() {
        let items = vec![
            AssistantItem::Reasoning {
                provider_id: ProviderId::new("mock"),
                payload: serde_json::json!({}),
            },
            AssistantItem::Text {
                phase: AssistantPhase::Commentary,
                text: String::from("Checking."),
            },
            AssistantItem::Reasoning {
                provider_id: ProviderId::new("mock"),
                payload: serde_json::json!({}),
            },
            AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: String::from("Done."),
            },
        ];
        let chunks = assistant_updates(&MessageId::new("message"), &items)
            .into_iter()
            .filter_map(|update| match update {
                acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk {
                    content: acp::ContentBlock::Text(text),
                    ..
                }) => Some(text.text),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(chunks, "Checking.\n\nDone.");
        assert_eq!(chunks, ConversationItem::Assistant { items }.display_text());
    }
}
