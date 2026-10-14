#![forbid(unsafe_code)]
//! What the script host, the sandbox and native commands exchange: permissions and policy,
//! script profiles and outcomes, state effects, and the requests commands make. Conversation
//! history and its records live in `kraai-types`.

mod effect;
mod error;
mod id;
pub mod image;
mod mcp;
mod permissions;
mod policy;
mod profile;
mod script;
mod web;

pub use effect::{ContextStateDelta, OpenedFilesOperation, StateEffectAck, StateEffectRequest};
pub use error::{DomainError, DomainErrorKind};
#[doc(hidden)]
pub use id::{__private, validate_id, validate_message_id};
pub use id::{CommandInvocationId, ScriptExecutionId};
pub use image::ImageAttachment;
pub use mcp::McpRequest;
pub use permissions::{SandboxCapabilities, SandboxCapability, SandboxCapabilityError};
pub use policy::{
    CapabilityPermissionRules, EscalationPolicy, PermissionResolution, ResolvedPermissions,
    SandboxPermissionSet,
};
pub use profile::{EnvironmentPolicy, NushellStartup, PathPolicy, ScriptProfileSnapshot};
pub use script::{
    ScriptExecutionOutcome, ScriptExecutionPhase, ScriptExecutionStatus, ScriptOutputStream,
};
pub use web::{WebSearchRequest, WebSearchResponse};
