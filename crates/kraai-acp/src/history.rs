use std::collections::HashSet;

use agent_client_protocol::{Result, v1::schema as acp};
use kraai_runtime::{RuntimeHandle, SessionSnapshot};
use kraai_types::{AssistantItem, ConversationItem, ScriptCall};

use crate::{content, error};

const HISTORY_PAGE: usize = 200;

pub(crate) async fn replay(
    runtime: &RuntimeHandle,
    snapshot: &SessionSnapshot,
    connection: &crate::transport::Connection,
) -> Result<()> {
    let mut tip = snapshot
        .session
        .tip_id
        .as_ref()
        .map(|id| kraai_types::MessageId::new(id.clone()));
    let mut history = snapshot.history.clone();
    let mut older = snapshot.older_history.clone();
    while let Some(from) = older {
        let page = runtime
            .get_history_page(snapshot.session.id.clone(), Some(from), HISTORY_PAGE)
            .await
            .map_err(error::runtime)?;
        history.extend(page.messages);
        older = page.older_history;
    }
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    while let Some(id) = tip {
        if !seen.insert(id.clone()) {
            return Err(error::internal("Conversation history contains a cycle"));
        }
        let message = history
            .get(&id)
            .ok_or_else(|| error::internal("Conversation history is incomplete"))?;
        messages.push(message);
        tip = message.parent_id.clone();
    }
    let session_id = acp::SessionId::new(snapshot.session.id.to_string());
    for message in messages.into_iter().rev() {
        for update in updates(runtime, message).await? {
            connection
                .send_notification_wait(acp::SessionNotification::new(session_id.clone(), update))
                .await?;
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
        ConversationItem::ScriptResult {
            call_id,
            output,
            outcome,
        } => {
            vec![content::tool_result(runtime, call_id.to_string(), output, *outcome).await?]
        }
        ConversationItem::FileContext { .. }
        | ConversationItem::Compaction { .. }
        | ConversationItem::System { .. } => Vec::new(),
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
                AssistantItem::ScriptCall(ScriptCall { call_id, input, .. }) => {
                    acp::SessionUpdate::ToolCall(content::script_call(
                        call_id.to_string(),
                        input.clone(),
                    ))
                }
                AssistantItem::Reasoning { .. } => return None,
            };
            has_prior_item = true;
            Some(update)
        })
        .collect()
}

#[cfg(test)]
mod tests;
