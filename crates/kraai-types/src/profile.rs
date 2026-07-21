use serde::{Deserialize, Serialize};

use crate::{CapabilityPermissionRules, EscalationPolicy, SandboxPermissionSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentPolicy {
    Minimal,
    Inherit,
    AllowList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NushellStartup {
    Clean,
    Inherit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathPolicy {
    Inherit,
    Packaged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptProfileSnapshot {
    pub id: String,
    pub commands: Vec<String>,
    pub permissions: SandboxPermissionSet,
    pub permission_rules: CapabilityPermissionRules,
    pub escalation_policy: EscalationPolicy,
    pub environment: EnvironmentPolicy,
    pub nushell_startup: NushellStartup,
    pub path: PathPolicy,
}
