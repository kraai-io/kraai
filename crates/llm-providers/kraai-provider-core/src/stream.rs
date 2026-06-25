use kraai_types::TokenUsage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStreamEvent {
    TextDelta(String),
    Usage(TokenUsage),
}
