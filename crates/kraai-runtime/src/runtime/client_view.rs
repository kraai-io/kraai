//! What clients see of history: everything except opaque provider payloads, which only the
//! provider that produced them can use and which can be large.

use kraai_types::{AssistantItem, ConversationItem, Message};

pub(crate) fn client_message(mut message: Message) -> Message {
    if let ConversationItem::Assistant { items } = &mut message.content {
        for item in items {
            strip_payload(item);
        }
    }
    message
}

pub(crate) fn client_item(mut item: AssistantItem) -> AssistantItem {
    strip_payload(&mut item);
    item
}

fn strip_payload(item: &mut AssistantItem) {
    if let AssistantItem::Reasoning { payload, .. } = item {
        *payload = serde_json::Value::Null;
    }
}
