use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxCapability {
    WorkspaceRead,
    HostRead,
    WorkspaceWrite,
    HostWrite,
    Network,
    NoSandbox,
}

impl SandboxCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceRead => "workspace-read",
            Self::HostRead => "host-read",
            Self::WorkspaceWrite => "workspace-write",
            Self::HostWrite => "host-write",
            Self::Network => "network",
            Self::NoSandbox => "no-sandbox",
        }
    }
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCapabilities(BTreeSet<SandboxCapability>);

impl SandboxCapabilities {
    pub fn workspace_read() -> Self {
        Self(BTreeSet::from([
            SandboxCapability::WorkspaceRead,
            SandboxCapability::Network,
        ]))
    }

    pub fn workspace_write() -> Self {
        Self(BTreeSet::from([
            SandboxCapability::WorkspaceRead,
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::Network,
        ]))
    }

    pub fn new(capabilities: impl IntoIterator<Item = SandboxCapability>) -> Self {
        let mut effective = capabilities.into_iter().collect::<BTreeSet<_>>();
        if effective.contains(&SandboxCapability::NoSandbox) {
            return Self(BTreeSet::from([SandboxCapability::NoSandbox]));
        }
        if effective.contains(&SandboxCapability::HostWrite) {
            effective.extend([
                SandboxCapability::HostRead,
                SandboxCapability::WorkspaceWrite,
                SandboxCapability::WorkspaceRead,
            ]);
        }
        if effective.contains(&SandboxCapability::WorkspaceWrite) {
            effective.insert(SandboxCapability::WorkspaceRead);
        }
        if effective.contains(&SandboxCapability::HostRead) {
            effective.insert(SandboxCapability::WorkspaceRead);
        }

        Self(effective)
    }

    pub fn contains(&self, capability: SandboxCapability) -> bool {
        self.0.contains(&SandboxCapability::NoSandbox) || self.0.contains(&capability)
    }

    pub fn is_unsandboxed(&self) -> bool {
        self.contains(SandboxCapability::NoSandbox)
    }

    pub fn iter(&self) -> impl Iterator<Item = SandboxCapability> + '_ {
        self.0.iter().copied()
    }
}

#[cfg(test)]
mod tests {
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
    fn no_sandbox_supersedes_other_capabilities() {
        let capabilities =
            SandboxCapabilities::new([SandboxCapability::NoSandbox, SandboxCapability::Network]);
        assert_eq!(
            capabilities.iter().collect::<Vec<_>>(),
            [SandboxCapability::NoSandbox]
        );
        assert!(capabilities.is_unsandboxed());
    }
}
