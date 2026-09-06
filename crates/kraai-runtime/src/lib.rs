#![forbid(unsafe_code)]
#![deny(clippy::all)]

mod api;
mod handle;
mod runtime;
mod settings;

pub use api::{
    AgentProfileCatalog, ContinueSessionOutcome, CreateSessionRequest, Event, EventCallback,
    FieldViolation, Model, OpenAiCodexAuthStatus, OpenAiCodexLoginState, PendingBrowserLogin,
    PendingDeviceCodeLogin, PendingScriptInfo, RuntimeError, RuntimeErrorKind, RuntimeEvent,
    RuntimeResult, RuntimeStartupState, Session, SessionActivity, SessionContextUsage,
    SessionSnapshot, SubmitMessageOutcome, WorkspaceState,
};
pub use handle::RuntimeHandle;
pub use kraai_provider_core::{
    DynamicValue as SettingsValue, FieldDefinition, FieldValueKind, ProviderDefinition,
};
pub use kraai_types::{
    AgentProfileSource, AgentProfileSummary, AgentProfileWarning, AgentProfilesState, Message,
    MessageId, TokenUsage,
};
pub use runtime::RuntimeBuilder;
pub use settings::{FieldValueEntry, ModelSettings, ProviderSettings, SettingsDocument};
