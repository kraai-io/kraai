use super::{
    CapabilityPermissionRules, EscalationPolicy, PermissionResolution, SandboxPermissionSet,
};
use crate::{SandboxCapabilities, SandboxCapability};

fn permissions(values: impl IntoIterator<Item = SandboxCapability>) -> SandboxPermissionSet {
    SandboxPermissionSet::new(values)
}

fn requested(values: impl IntoIterator<Item = SandboxCapability>) -> SandboxCapabilities {
    SandboxCapabilities::new(values)
}

#[test]
fn already_granted_capabilities_are_semantic_no_ops() {
    let defaults = permissions([SandboxCapability::HostRead]);
    let resolution = defaults.resolve(
        &requested([SandboxCapability::WorkspaceRead]),
        &CapabilityPermissionRules::default(),
        EscalationPolicy::Deny,
    );
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
    let resolution = defaults.resolve(
        &requested([
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::Network,
        ]),
        &rules,
        EscalationPolicy::Allow,
    );
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
    let resolution = defaults.resolve(
        &requested([
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::Network,
        ]),
        &CapabilityPermissionRules::default(),
        EscalationPolicy::Prompt,
    );
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
    let resolution = defaults.resolve(
        &requested([SandboxCapability::NoSandbox]),
        &CapabilityPermissionRules::default(),
        EscalationPolicy::Allow,
    );
    let PermissionResolution::Allowed(resolved) = resolution else {
        panic!("no-sandbox should be allowed");
    };
    assert!(resolved.effective().is_unsandboxed());
    assert_eq!(resolved.additions(), &[SandboxCapability::NoSandbox]);
}
