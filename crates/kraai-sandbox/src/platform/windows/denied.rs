use std::path::{Path, PathBuf};

/// The AppContainer can only open what a grant reaches, so only denied paths inside a grant
/// need their entries stripped. Inside a denied path the deepest denied path decides, and
/// grants strictly inside it keep their own access: any writable one leaves the object alone,
/// read-only ones keep only their own entries.
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

    /// The indices of the grants whose entries `path` keeps, or `None` when `path` is not
    /// hidden.
    pub(super) fn kept(&self, path: &Path) -> Option<Vec<usize>> {
        let denied = self
            .denied
            .iter()
            .filter(|denied| path.starts_with(denied))
            .max_by_key(|denied| denied.components().count())?;
        let mut kept = Vec::new();
        for (index, (grant, writable)) in self.grants.iter().enumerate() {
            if grant != denied && grant.starts_with(denied) && path.starts_with(grant) {
                if *writable {
                    return None;
                }
                kept.push(index);
            }
        }
        Some(kept)
    }

    pub(super) fn prunes(&self, path: &Path) -> bool {
        self.kept(path).is_none()
            && !self
                .denied
                .iter()
                .any(|denied| denied != path && denied.starts_with(path))
    }
}

#[cfg(test)]
#[path = "denied/tests.rs"]
mod tests;
