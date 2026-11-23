use kraai_provider_core::ProviderRequest;
use kraai_types::{AssistantItem, ContentPart, ConversationItem, MessageContent, ScriptCall};

const TRUNCATED_OUTPUT: &str = "Output exceeded the available model context and was truncated";

pub(crate) fn trim_tool_outputs(request: &mut ProviderRequest, context_window: Option<usize>) {
    let Some(limit) = context_window else { return };
    let mut bytes = request
        .messages
        .iter()
        .map(visible_bytes)
        .fold(0usize, usize::saturating_add);
    for item in request.messages.iter_mut().rev() {
        if bytes.div_ceil(4) <= limit {
            break;
        }
        if matches!(item, ConversationItem::System { .. }) {
            continue;
        }
        let ConversationItem::ScriptResult { output, .. } = item else {
            break;
        };
        let mut truncated = MessageContent::from(TRUNCATED_OUTPUT);
        truncated
            .0
            .extend(output.images().map(ContentPart::omitted_image));
        bytes = bytes
            .saturating_sub(output.display_text().len())
            .saturating_add(truncated.display_text().len());
        *output = truncated;
    }
}

fn encrypted_bytes(payload: &serde_json::Value) -> usize {
    payload
        .get("encrypted_content")
        .and_then(serde_json::Value::as_str)
        .map_or(0, |text| {
            text.len()
                .saturating_mul(3)
                .saturating_div(4)
                .saturating_sub(650)
        })
}

fn visible_bytes(item: &ConversationItem) -> usize {
    match item {
        ConversationItem::System { text } | ConversationItem::FileContext { text } => text.len(),
        ConversationItem::User { content } => content.display_text().len(),
        ConversationItem::Compaction { payload, .. } => encrypted_bytes(payload),
        ConversationItem::ScriptResult { output, .. } => output.display_text().len(),
        ConversationItem::Assistant { items } => items
            .iter()
            .map(|item| match item {
                AssistantItem::Text { text, .. } => text.len(),
                AssistantItem::Reasoning { payload, .. } => encrypted_bytes(payload),
                AssistantItem::ScriptCall(ScriptCall { name, input, .. }) => {
                    name.len().saturating_add(input.len())
                }
            })
            .fold(0usize, usize::saturating_add),
    }
}

#[cfg(test)]
mod tests;
