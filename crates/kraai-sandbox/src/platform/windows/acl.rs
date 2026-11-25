#![expect(
    unsafe_code,
    reason = "Windows DACL edits use pinned file handles and OS-allocated security descriptors"
)]

mod handle;
mod mutation;

use std::fs::{File, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use super::denied::{Denials, Deny};
use super::identity::Sid;
use crate::SandboxError;

const MAX_ACL_ENTRIES: usize = 65_536;

#[derive(Clone, Copy)]
pub(super) enum Access {
    Read,
    Write,
}

#[derive(Clone, Copy)]
pub(super) enum Entry {
    Allow(Access),
    Deny(Deny),
}

/// Denied trees keep no handles open, so Kraai can still replace and delete its own files in
/// them while a script runs.
#[derive(Debug)]
pub(super) struct Grants {
    sid: Sid,
    files: Vec<File>,
    roots: Vec<PathBuf>,
    denials: Option<Denials>,
}

impl Grants {
    pub(super) fn new(sid: Sid) -> Self {
        Self {
            sid,
            files: Vec::new(),
            roots: Vec::new(),
            denials: None,
        }
    }

    pub(super) fn grant(&mut self, path: &Path, access: Access) -> Result<(), SandboxError> {
        self.grant_with_limit(path, access, MAX_ACL_ENTRIES)
    }

    fn grant_with_limit(
        &mut self,
        path: &Path,
        access: Access,
        limit: usize,
    ) -> Result<(), SandboxError> {
        let _lock = super::mutation_lock::Lock::acquire()?;
        self.roots.push(path.to_path_buf());
        visit_tree(path, |file| {
            if self.files.len() >= limit {
                return Err(entry_limit(path, limit));
            }
            handle::ensure_single_link(&file).map_err(|error| failure(path, &error.to_string()))?;
            mutation::update(&file, &self.sid, Some(Entry::Allow(access)))
                .map_err(|error| failure(path, &error.to_string()))?;
            self.files.push(file);
            Ok(())
        })
    }

    pub(super) fn deny(&mut self, denials: Denials) -> Result<(), SandboxError> {
        let _lock = super::mutation_lock::Lock::acquire()?;
        let denials = &*self.denials.insert(denials);
        for root in denials.roots() {
            walk(
                root,
                MAX_ACL_ENTRIES,
                |path| denials.prunes(path),
                |path, file| {
                    let Some(deny) = denials.entry(path) else {
                        return Ok(());
                    };
                    mutation::update(&file, &self.sid, Some(Entry::Deny(deny)))
                        .map_err(|error| failure(path, &error.to_string()))
                },
            )?;
        }
        Ok(())
    }
}

impl Grants {
    pub(super) fn cleanup(&mut self) -> Result<(), SandboxError> {
        let _lock = super::mutation_lock::Lock::acquire()?;
        let mut failure = None;
        for file in &self.files {
            if let Err(error) = mutation::update(file, &self.sid, None) {
                failure.get_or_insert(error);
            }
        }
        for root in &self.roots {
            if root.exists()
                && let Err(error) =
                    visit_tree(root, |file| mutation::update(&file, &self.sid, None))
            {
                failure.get_or_insert(error);
            }
        }
        if let Some(denials) = &self.denials {
            for root in denials.roots() {
                if root.exists()
                    && let Err(error) = walk(
                        root,
                        MAX_ACL_ENTRIES,
                        |path| denials.prunes(path),
                        |_, file| mutation::update(&file, &self.sid, None),
                    )
                {
                    failure.get_or_insert(error);
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.files.clear();
        self.roots.clear();
        self.denials = None;
        Ok(())
    }
}

impl Drop for Grants {
    fn drop(&mut self) {
        if !self.files.is_empty() || !self.roots.is_empty() || self.denials.is_some() {
            let _ = self.cleanup();
        }
    }
}

/// `path` is granted after everything below it, so its entry marks a completed walk.
pub(super) fn grant_persistent(path: &Path, sid: &Sid, access: Access) -> Result<(), SandboxError> {
    let _lock = super::mutation_lock::Lock::acquire()?;
    let root =
        handle::open_by_id(&open(path)?).map_err(|error| failure(path, &error.to_string()))?;
    if mutation::grants(&root, sid, access)? {
        return Ok(());
    }
    let mut entries = 0;
    let mut walked_root = None;
    visit_tree(path, |file| {
        if entries >= MAX_ACL_ENTRIES {
            return Err(entry_limit(path, MAX_ACL_ENTRIES));
        }
        entries += 1;
        handle::ensure_single_link(&file).map_err(|error| failure(path, &error.to_string()))?;
        if walked_root.is_none() {
            walked_root = Some(file);
            return Ok(());
        }
        mutation::update(&file, sid, Some(Entry::Allow(access)))
            .map_err(|error| failure(path, &error.to_string()))
    })?;
    let walked_root = walked_root.ok_or_else(|| failure(path, "sandbox root was not visited"))?;
    mutation::update(&walked_root, sid, Some(Entry::Allow(access)))
        .map_err(|error| failure(path, &error.to_string()))
}

fn visit_tree(
    root: &Path,
    visit: impl FnMut(File) -> Result<(), SandboxError>,
) -> Result<(), SandboxError> {
    visit_tree_with_limit(root, MAX_ACL_ENTRIES, visit)
}

fn visit_tree_with_limit(
    root: &Path,
    limit: usize,
    mut visit: impl FnMut(File) -> Result<(), SandboxError>,
) -> Result<(), SandboxError> {
    walk(root, limit, |_| false, |_, file| visit(file))
}

fn walk(
    root: &Path,
    limit: usize,
    prune: impl Fn(&Path) -> bool,
    mut visit: impl FnMut(&Path, File) -> Result<(), SandboxError>,
) -> Result<(), SandboxError> {
    let mut pins = pin_ancestors(root)?;
    let mut pending = vec![(root.to_path_buf(), true)];
    let mut entries = 0;
    while let Some((path, is_root)) = pending.pop() {
        if !is_root && prune(&path) {
            continue;
        }
        if entries >= limit {
            return Err(entry_limit(root, limit));
        }
        entries += 1;
        let pin = open(&path)?;
        if is_reparse(&pin)? {
            if is_root {
                return Err(failure(&path, "reparse points cannot be sandbox roots"));
            }
            continue;
        }
        let directory = pin
            .metadata()
            .map_err(|error| failure(&path, &error.to_string()))?
            .is_dir();
        let file = handle::open_by_id(&pin).map_err(|error| failure(&path, &error.to_string()))?;
        visit(&path, file)?;
        if directory {
            pins.push(pin);
            for entry in
                std::fs::read_dir(&path).map_err(|error| failure(&path, &error.to_string()))?
            {
                if entries + pending.len() >= limit {
                    return Err(entry_limit(root, limit));
                }
                let child = entry
                    .map_err(|error| failure(&path, &error.to_string()))?
                    .path();
                pending.push((child, false));
            }
        }
    }
    Ok(())
}

fn pin_ancestors(path: &Path) -> Result<Vec<File>, SandboxError> {
    let mut ancestors = path.ancestors().skip(1).collect::<Vec<_>>();
    ancestors.reverse();
    let mut pins = Vec::new();
    for ancestor in ancestors {
        let pin = open(ancestor)?;
        if is_reparse(&pin)? {
            return Err(failure(ancestor, "reparse point in sandbox root ancestry"));
        }
        pins.push(pin);
    }
    Ok(pins)
}

fn open(path: &Path) -> Result<File, SandboxError> {
    let mut options = OpenOptions::new();
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    match options
        .access_mode(FILE_READ_ATTRIBUTES | FILE_READ_DATA)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) =>
        {
            let file = options
                .access_mode(FILE_READ_ATTRIBUTES)
                .open(path)
                .map_err(|error| failure(path, &error.to_string()))?;
            if file
                .metadata()
                .map_err(|error| failure(path, &error.to_string()))?
                .is_dir()
            {
                return Err(failure(
                    path,
                    "directory traversal requires list access to pin its name",
                ));
            }
            // Leaf objects are reopened by ID, so replacing their names cannot redirect an ACL edit.
            Ok(file)
        }
        Err(error) => Err(failure(path, &error.to_string())),
    }
}

fn is_reparse(file: &File) -> Result<bool, SandboxError> {
    file.metadata()
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .map_err(|error| {
            SandboxError::SandboxUnavailable(format!("unable to inspect sandbox handle: {error}"))
        })
}

fn failure(path: &Path, message: &str) -> SandboxError {
    SandboxError::SandboxUnavailable(format!("unable to secure '{}': {message}", path.display()))
}

fn entry_limit(path: &Path, limit: usize) -> SandboxError {
    failure(
        path,
        &format!(
            "Windows sandbox ACL entry limit ({limit}) exceeded; reduce the workspace or runtime roots"
        ),
    )
}

#[cfg(test)]
mod tests;
