use kraai_types::{AssistantPhase, TokenUsage, ToolCallId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStreamEvent {
    Compaction {
        payload: serde_json::Value,
    },
    Reasoning {
        payload: serde_json::Value,
    },
    TextDelta {
        item_id: String,
        phase: AssistantPhase,
        delta: String,
    },
    ScriptCall {
        call_id: ToolCallId,
        name: String,
        input: String,
    },
    Usage(TokenUsage),
}
