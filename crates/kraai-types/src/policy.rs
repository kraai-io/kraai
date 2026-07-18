use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{SandboxCapabilities, SandboxCapability, SandboxCapabilityError};

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
    pub fn workspace_read() -> Self {
        Self {
            capabilities: SandboxCapabilities::workspace_read(),
        }
    }

    pub fn workspace_write() -> Self {
        Self {
            capabilities: SandboxCapabilities::workspace_write(),
        }
    }

    pub fn new(
        capabilities: impl IntoIterator<Item = SandboxCapability>,
    ) -> Result<Self, SandboxCapabilityError> {
        Ok(Self {
            capabilities: SandboxCapabilities::new(capabilities)?,
        })
    }

    pub fn capabilities(&self) -> &SandboxCapabilities {
        &self.capabilities
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
    ) -> Result<PermissionResolution, SandboxCapabilityError> {
        let additions = requested_additions(&self.capabilities, requested);
        if additions.is_empty() {
            return Ok(PermissionResolution::Allowed(ResolvedPermissions {
                effective: self.capabilities.clone(),
                additions,
            }));
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
            return Ok(PermissionResolution::Denied { denied });
        }

        let effective = merge_capabilities(&self.capabilities, requested)?;
        let resolved = ResolvedPermissions {
            effective,
            additions,
        };
        if prompt {
            Ok(PermissionResolution::Prompt {
                candidate: resolved,
            })
        } else {
            Ok(PermissionResolution::Allowed(resolved))
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
) -> Result<SandboxCapabilities, SandboxCapabilityError> {
    if requested.is_unsandboxed() {
        return SandboxCapabilities::new([SandboxCapability::NoSandbox]);
    }
    let combined = defaults
        .iter()
        .chain(requested.iter())
        .collect::<BTreeSet<_>>();
    SandboxCapabilities::new(combined)
}

#[cfg(test)]
#[expect(
    clippy::panic,
    reason = "permission tests use direct failure messages and exhaustive variant assertions"
)]
mod tests {
    use super::{
        CapabilityPermissionRules, EscalationPolicy, PermissionResolution, SandboxPermissionSet,
    };
    use crate::{SandboxCapabilities, SandboxCapability};

    fn permissions(values: impl IntoIterator<Item = SandboxCapability>) -> SandboxPermissionSet {
        SandboxPermissionSet::new(values)
            .unwrap_or_else(|error| panic!("invalid permission set: {error}"))
    }

    fn requested(values: impl IntoIterator<Item = SandboxCapability>) -> SandboxCapabilities {
        SandboxCapabilities::new(values)
            .unwrap_or_else(|error| panic!("invalid requested capabilities: {error}"))
    }

    #[test]
    fn already_granted_capabilities_are_semantic_no_ops() {
        let defaults = permissions([SandboxCapability::HostRead]);
        let resolution = defaults
            .resolve(
                &requested([SandboxCapability::WorkspaceRead]),
                &CapabilityPermissionRules::default(),
                EscalationPolicy::Deny,
            )
            .unwrap_or_else(|error| panic!("resolution failed: {error}"));
        let PermissionResolution::Allowed(resolved) = resolution else {
            panic!("semantic no-op should be allowed");
        };
        assert!(resolved.additions().is_empty());
        assert!(resolved.effective().contains(SandboxCapability::HostRead));
    }

    #[test]
    fn rules_precede_fallback_and_denial_precedes_prompt() {
        let defaults = permissions([SandboxCapability::WorkspaceRead]);
        let rules = CapabilityPermissionRules::new([
            (SandboxCapability::WorkspaceWrite, EscalationPolicy::Prompt),
            (SandboxCapability::Network, EscalationPolicy::Deny),
        ]);
        let resolution = defaults
            .resolve(
                &requested([
                    SandboxCapability::WorkspaceWrite,
                    SandboxCapability::Network,
                ]),
                &rules,
                EscalationPolicy::Allow,
            )
            .unwrap_or_else(|error| panic!("resolution failed: {error}"));
        assert_eq!(
            resolution,
            PermissionResolution::Denied {
                denied: vec![SandboxCapability::Network]
            }
        );
    }

    #[test]
    fn multiple_prompted_capabilities_produce_one_candidate() {
        let defaults = permissions([SandboxCapability::WorkspaceRead]);
        let resolution = defaults
            .resolve(
                &requested([
                    SandboxCapability::WorkspaceWrite,
                    SandboxCapability::Network,
                ]),
                &CapabilityPermissionRules::default(),
                EscalationPolicy::Prompt,
            )
            .unwrap_or_else(|error| panic!("resolution failed: {error}"));
        let PermissionResolution::Prompt { candidate } = resolution else {
            panic!("request should require one prompt");
        };
        assert_eq!(candidate.additions().len(), 2);
        assert!(
            candidate
                .effective()
                .contains(SandboxCapability::WorkspaceWrite)
        );
        assert!(candidate.effective().contains(SandboxCapability::Network));
    }

    #[test]
    fn allowed_no_sandbox_replaces_the_default_sandbox() {
        let defaults = permissions([SandboxCapability::WorkspaceRead]);
        let resolution = defaults
            .resolve(
                &requested([SandboxCapability::NoSandbox]),
                &CapabilityPermissionRules::default(),
                EscalationPolicy::Allow,
            )
            .unwrap_or_else(|error| panic!("resolution failed: {error}"));
        let PermissionResolution::Allowed(resolved) = resolution else {
            panic!("no-sandbox should be allowed");
        };
        assert!(resolved.effective().is_unsandboxed());
        assert_eq!(resolved.additions(), &[SandboxCapability::NoSandbox]);
    }
}
