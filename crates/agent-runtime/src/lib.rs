#![forbid(unsafe_code)]
#![deny(clippy::all)]

mod api;
mod handle;
mod runtime;
mod settings;

pub use api::{
    Event, EventCallback, Model, OpenAiCodexAuthStatus, OpenAiCodexLoginState, PendingBrowserLogin,
    PendingDeviceCodeLogin, PendingToolInfo, Session, WorkspaceState,
};
pub use handle::RuntimeHandle;
pub use provider_core::{
    DynamicValue as SettingsValue, FieldDefinition, FieldValueKind, ProviderDefinition,
};
pub use runtime::RuntimeBuilder;
pub use settings::{FieldValueEntry, ModelSettings, ProviderSettings, SettingsDocument};
pub use types::{
    AgentProfileSource, AgentProfileSummary, AgentProfileWarning, AgentProfilesState, Message,
    MessageId,
};
