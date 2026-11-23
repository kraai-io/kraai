use super::*;
use kraai_types::ScriptCall;
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
    let call = AssistantItem::ScriptCall(ScriptCall {
        call_id: ToolCallId::new("call"),
        name: "kraai_nushell".into(),
        input: "ls".into(),
        arguments_error: None,
        rewrite: None,
    });
    let result = ConversationItem::ScriptResult {
        outcome: kraai_execution_types::ScriptExecutionOutcome {
            status: kraai_execution_types::ScriptExecutionStatus::Completed,
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
