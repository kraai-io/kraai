use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxCapability {
    WorkspaceRead,
    HostRead,
    WorkspaceWrite,
    MetadataWrite,
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
            Self::MetadataWrite => "metadata-write",
            Self::HostWrite => "host-write",
            Self::Network => "network",
            Self::NoSandbox => "no-sandbox",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SandboxCapabilities(BTreeSet<SandboxCapability>);

impl SandboxCapabilities {
    pub fn workspace_read() -> Self {
        Self(BTreeSet::from([SandboxCapability::WorkspaceRead]))
    }

    pub fn workspace_write() -> Self {
        Self(BTreeSet::from([
            SandboxCapability::WorkspaceRead,
            SandboxCapability::WorkspaceWrite,
        ]))
    }

    pub fn new(
        capabilities: impl IntoIterator<Item = SandboxCapability>,
    ) -> Result<Self, SandboxCapabilityError> {
        let requested = capabilities.into_iter().collect::<BTreeSet<_>>();
        if requested.contains(&SandboxCapability::NoSandbox) && requested.len() != 1 {
            return Err(SandboxCapabilityError::NoSandboxMustBeExclusive);
        }

        let mut effective = requested;
        if effective.contains(&SandboxCapability::HostWrite) {
            effective.extend([
                SandboxCapability::HostRead,
                SandboxCapability::MetadataWrite,
                SandboxCapability::WorkspaceWrite,
                SandboxCapability::WorkspaceRead,
            ]);
        }
        if effective.contains(&SandboxCapability::MetadataWrite) {
            effective.extend([
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

        Ok(Self(effective))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxCapabilityError {
    NoSandboxMustBeExclusive,
}

impl std::fmt::Display for SandboxCapabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSandboxMustBeExclusive => {
                write!(
                    f,
                    "no-sandbox must be the only requested sandbox capability"
                )
            }
        }
    }
}

impl std::error::Error for SandboxCapabilityError {}

#[cfg(test)]
#[expect(
    clippy::panic,
    reason = "capability unit tests require direct construction assertions"
)]
mod tests {
    use super::{SandboxCapabilities, SandboxCapability, SandboxCapabilityError};

    #[test]
    fn computes_semantic_capability_closure() {
        let capabilities = SandboxCapabilities::new([SandboxCapability::HostWrite])
            .unwrap_or_else(|error| panic!("unexpected capability error: {error}"));

        for expected in [
            SandboxCapability::HostRead,
            SandboxCapability::WorkspaceRead,
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::MetadataWrite,
            SandboxCapability::HostWrite,
        ] {
            assert!(capabilities.contains(expected));
        }
        assert!(!capabilities.contains(SandboxCapability::Network));
    }

    #[test]
    fn no_sandbox_is_exclusive() {
        assert_eq!(
            SandboxCapabilities::new([SandboxCapability::NoSandbox, SandboxCapability::Network,]),
            Err(SandboxCapabilityError::NoSandboxMustBeExclusive)
        );
    }
}
