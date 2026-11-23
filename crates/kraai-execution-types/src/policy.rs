use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{SandboxCapabilities, SandboxCapability};

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EscalationPolicy {
    Deny,
    Prompt,
    Allow,
}

impl EscalationPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Prompt => "prompt",
            Self::Allow => "allow",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPermissionSet {
    capabilities: SandboxCapabilities,
}

impl SandboxPermissionSet {
    pub fn new(capabilities: impl IntoIterator<Item = SandboxCapability>) -> Self {
        Self {
            capabilities: SandboxCapabilities::new(capabilities),
        }
    }

    pub fn capabilities(&self) -> &SandboxCapabilities {
        &self.capabilities
    }

    pub fn into_capabilities(self) -> SandboxCapabilities {
        self.capabilities
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityPermissionRules(BTreeMap<SandboxCapability, EscalationPolicy>);

impl CapabilityPermissionRules {
    pub fn new(rules: impl IntoIterator<Item = (SandboxCapability, EscalationPolicy)>) -> Self {
        Self(rules.into_iter().collect())
    }

    pub fn get(&self, capability: SandboxCapability) -> Option<EscalationPolicy> {
        self.0.get(&capability).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionResolution {
    Denied { denied: Vec<SandboxCapability> },
    Prompt { candidate: ResolvedPermissions },
    Allowed(ResolvedPermissions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPermissions {
    effective: SandboxCapabilities,
    additions: Vec<SandboxCapability>,
}

impl ResolvedPermissions {
    pub fn effective(&self) -> &SandboxCapabilities {
        &self.effective
    }

    pub fn additions(&self) -> &[SandboxCapability] {
        &self.additions
    }
}

impl SandboxPermissionSet {
    pub fn resolve(
        &self,
        requested: &SandboxCapabilities,
        rules: &CapabilityPermissionRules,
        fallback: EscalationPolicy,
    ) -> PermissionResolution {
        let additions = requested_additions(&self.capabilities, requested);
        if additions.is_empty() {
            return PermissionResolution::Allowed(ResolvedPermissions {
                effective: self.capabilities.clone(),
                additions,
            });
        }

        let mut denied = Vec::new();
        let mut prompt = false;
        for capability in &additions {
            match rules.get(*capability).unwrap_or(fallback) {
                EscalationPolicy::Deny => denied.push(*capability),
                EscalationPolicy::Prompt => prompt = true,
                EscalationPolicy::Allow => {}
            }
        }
        if !denied.is_empty() {
            return PermissionResolution::Denied { denied };
        }

        let effective = merge_capabilities(&self.capabilities, requested);
        let resolved = ResolvedPermissions {
            effective,
            additions,
        };
        if prompt {
            PermissionResolution::Prompt {
                candidate: resolved,
            }
        } else {
            PermissionResolution::Allowed(resolved)
        }
    }
}

fn requested_additions(
    defaults: &SandboxCapabilities,
    requested: &SandboxCapabilities,
) -> Vec<SandboxCapability> {
    if requested.is_unsandboxed() && !defaults.is_unsandboxed() {
        return vec![SandboxCapability::NoSandbox];
    }
    requested
        .iter()
        .filter(|capability| !defaults.contains(*capability))
        .collect()
}

fn merge_capabilities(
    defaults: &SandboxCapabilities,
    requested: &SandboxCapabilities,
) -> SandboxCapabilities {
    SandboxCapabilities::new(defaults.iter().chain(requested.iter()))
}

#[cfg(test)]
mod tests;
