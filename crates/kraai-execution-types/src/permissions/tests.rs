use super::{SandboxCapabilities, SandboxCapability};

#[test]
fn computes_semantic_capability_closure() {
    let capabilities = SandboxCapabilities::new([SandboxCapability::HostWrite]);

    for expected in [
        SandboxCapability::HostRead,
        SandboxCapability::WorkspaceRead,
        SandboxCapability::WorkspaceWrite,
        SandboxCapability::HostWrite,
    ] {
        assert!(capabilities.contains(expected));
    }
    assert!(!capabilities.contains(SandboxCapability::Network));
}

#[test]
fn without_implied_keeps_capabilities_no_other_one_grants() {
    let capabilities = SandboxCapabilities::new([
        SandboxCapability::HostRead,
        SandboxCapability::WorkspaceWrite,
        SandboxCapability::Network,
    ]);
    assert_eq!(
        capabilities.without_implied().collect::<Vec<_>>(),
        [
            SandboxCapability::HostRead,
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::Network,
        ]
    );
    let capabilities = SandboxCapabilities::new([SandboxCapability::HostWrite]);
    assert_eq!(
        capabilities.without_implied().collect::<Vec<_>>(),
        [SandboxCapability::HostWrite]
    );
}

#[test]
fn no_sandbox_supersedes_other_capabilities() {
    let capabilities =
        SandboxCapabilities::new([SandboxCapability::NoSandbox, SandboxCapability::Network]);
    assert_eq!(
        capabilities.iter().collect::<Vec<_>>(),
        [SandboxCapability::NoSandbox]
    );
    assert!(capabilities.is_unsandboxed());
}
