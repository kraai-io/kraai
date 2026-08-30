use kraai_types::{AssistantPhase, TokenUsage, ToolCallId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStreamEvent {
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
