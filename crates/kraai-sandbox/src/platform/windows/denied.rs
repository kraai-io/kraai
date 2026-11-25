use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Deny {
    All,
    Write,
}

/// The AppContainer can only open what a grant reaches, so only denied paths inside a grant
/// need deny entries. Inside a denied path the deepest denied path decides, and grants
/// strictly inside it keep their own access: any writable one leaves the object alone,
/// read-only ones only lose write access.
#[derive(Debug)]
pub(super) struct Denials {
    denied: Vec<PathBuf>,
    grants: Vec<(PathBuf, bool)>,
    roots: Vec<PathBuf>,
}

impl Denials {
    pub(super) fn new(denied: Vec<PathBuf>, grants: Vec<(PathBuf, bool)>) -> Self {
        let mut covered = denied
            .iter()
            .filter(|path| grants.iter().any(|(grant, _)| path.starts_with(grant)))
            .collect::<Vec<_>>();
        covered.sort_by_key(|path| path.components().count());
        let mut roots = Vec::<PathBuf>::new();
        for path in covered {
            if !roots.iter().any(|root| path.starts_with(root)) {
                roots.push(path.clone());
            }
        }
        Self {
            denied,
            grants,
            roots,
        }
    }

    pub(super) fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub(super) fn entry(&self, path: &Path) -> Option<Deny> {
        let denied = self
            .denied
            .iter()
            .filter(|denied| path.starts_with(denied))
            .max_by_key(|denied| denied.components().count())?;
        let mut deny = Deny::All;
        for (grant, writable) in &self.grants {
            if grant != denied && grant.starts_with(denied) && path.starts_with(grant) {
                if *writable {
                    return None;
                }
                deny = Deny::Write;
            }
        }
        Some(deny)
    }

    pub(super) fn prunes(&self, path: &Path) -> bool {
        self.entry(path).is_none()
            && !self
                .denied
                .iter()
                .any(|denied| denied != path && denied.starts_with(path))
    }
}

#[cfg(test)]
#[path = "denied/tests.rs"]
mod tests;
