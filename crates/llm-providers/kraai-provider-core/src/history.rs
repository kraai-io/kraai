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
                        _ => true,
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
                _ => {}
            }
            [Some(message), following]
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kraai_types::ToolCallId;
    use serde_json::json;

    #[test]
    fn provider_switch_discards_only_foreign_opaque_items() {
        let own = ProviderId::new("own");
        let other = ProviderId::new("other");
        let reasoning = AssistantItem::Reasoning {
            provider_id: own.clone(),
            payload: json!({"opaque": "own reasoning"}),
        };
        let compaction = ConversationItem::Compaction {
            provider_id: own.clone(),
            payload: json!({"opaque": "own compaction"}),
        };
        let call = AssistantItem::ScriptCall {
            call_id: ToolCallId::new("call"),
            name: "kraai_nushell".into(),
            input: "ls".into(),
        };
        let result = ConversationItem::ScriptResult {
            outcome: kraai_types::ScriptExecutionOutcome {
                status: kraai_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
            call_id: ToolCallId::new("call"),
            output: "files".into(),
        };
        let prepared = prepare_history(
            [
                ConversationItem::Compaction {
                    provider_id: other.clone(),
                    payload: json!({"opaque": "foreign compaction"}),
                },
                compaction.clone(),
                ConversationItem::Assistant {
                    items: vec![
                        AssistantItem::Reasoning {
                            provider_id: other,
                            payload: json!({"opaque": "foreign reasoning"}),
                        },
                        reasoning.clone(),
                        call.clone(),
                    ],
                },
                result.clone(),
            ],
            &own,
        )
        .collect::<Vec<_>>();
        assert_eq!(
            prepared,
            vec![
                compaction,
                ConversationItem::Assistant {
                    items: vec![reasoning, call]
                },
                result
            ]
        );
    }
}
