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

    pub fn without_implied(&self) -> impl Iterator<Item = SandboxCapability> + '_ {
        self.iter().filter(|capability| {
            let others = self.iter().filter(|other| other != capability);
            !Self::new(others).0.contains(capability)
        })
    }
}

#[cfg(test)]
mod tests;
