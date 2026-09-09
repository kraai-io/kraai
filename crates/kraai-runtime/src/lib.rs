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

/// Runs a hidden runtime subprocess mode when requested by a child Kraai process.
///
/// Frontend executables that opt into `RuntimeBuilder::use_current_executable_as_nushell_host`
/// must call this before parsing their own command-line arguments.
pub fn run_internal_process() -> Option<i32> {
    (std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(
            kraai_nushell_runtime::INTERNAL_HOST_ARGUMENT,
        )))
    .then(kraai_nushell_runtime::run_host_process)
}
