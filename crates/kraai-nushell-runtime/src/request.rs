use std::collections::BTreeMap;
use std::path::PathBuf;

use kraai_execution_types::{
    NushellStartup, SandboxCapabilities, SandboxCapability, ScriptExecutionId,
};
use serde::{Deserialize, Serialize};

pub const HOST_PROTOCOL_VERSION: u32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostRequest {
    pub protocol_version: u32,
    pub execution_id: ScriptExecutionId,
    pub source: Vec<u8>,
    pub workspace_root: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub active_commands: Vec<String>,
    pub nushell_startup: NushellStartup,
    pub startup_files: Vec<crate::StartupFile>,
    pub event_secret: [u8; 32],
    pub(crate) output_marker: crate::rendering::Marker,
    pub startup_restriction: StartupRestriction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartupRestriction {
    None,
    Network,
    HostIpc,
}

impl StartupRestriction {
    pub fn for_capabilities(capabilities: &SandboxCapabilities) -> Self {
        if capabilities.is_unsandboxed() {
            Self::None
        } else if capabilities.contains(SandboxCapability::Network) {
            Self::HostIpc
        } else {
            Self::Network
        }
    }
}
