use kraai_types::{AssistantItem, ConversationItem, ProviderId};

use crate::images::script_result_images;

pub fn prepare_history(
    messages: impl IntoIterator<Item = ConversationItem>,
    provider_id: &ProviderId,
) -> impl Iterator<Item = ConversationItem> {
    let provider_id = provider_id.clone();
    messages
        .into_iter()
        .flat_map(move |mut message| {
            let mut following = None;
            match &mut message {
                ConversationItem::Compaction {
                    provider_id: source,
                    ..
                } if *source != provider_id => {
                    return [None, None];
                }
                ConversationItem::Assistant { items } => {
                    items.retain(|item| match item {
                        AssistantItem::Reasoning {
                            provider_id: source,
                            ..
                        } => *source == provider_id,
                        AssistantItem::Text { .. } | AssistantItem::ScriptCall(_) => true,
                    });
                }
                ConversationItem::ScriptResult {
                    call_id, output, ..
                } if output.has_images() => {
                    let text = output.display_text().into_owned();
                    following = script_result_images(std::mem::take(output), call_id)
                        .map(|content| ConversationItem::User { content });
                    *output = text.into();
                }
                ConversationItem::FileContext { .. }
                | ConversationItem::Compaction { .. }
                | ConversationItem::System { .. }
                | ConversationItem::User { .. }
                | ConversationItem::ScriptResult { .. } => {}
            }
            [Some(message), following]
        })
        .flatten()
}

#[cfg(test)]
mod tests;
