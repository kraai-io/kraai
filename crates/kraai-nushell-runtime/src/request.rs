use std::collections::BTreeMap;
use std::path::PathBuf;

use kraai_types::{NushellStartup, ScriptExecutionId};
use serde::{Deserialize, Serialize};

pub const HOST_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostRequest {
    pub protocol_version: u32,
    pub execution_id: ScriptExecutionId,
    pub source: Vec<u8>,
    pub workspace_root: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub active_commands: Vec<String>,
    pub nushell_startup: NushellStartup,
    pub event_secret: [u8; 32],
}
