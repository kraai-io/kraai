#![expect(
    unsafe_code,
    reason = "Windows DACL edits use pinned file handles and OS-allocated security descriptors"
)]

mod handle;
mod mutation;

use std::fs::{File, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ,
};

use crate::SandboxError;

const MAX_ACL_ENTRIES: usize = 65_536;

#[derive(Clone, Copy)]
pub(super) enum Access {
    Read,
    Write,
}

#[derive(Debug)]
pub(super) struct Grants {
    sid: Vec<u32>,
    files: Vec<File>,
    roots: Vec<PathBuf>,
}

impl Grants {
    pub(super) fn new(bytes: Vec<u8>) -> Self {
        let sid = bytes
            .chunks(4)
            .map(|chunk| {
                let mut word = [0_u8; 4];
                for (destination, source) in word.iter_mut().zip(chunk) {
                    *destination = *source;
                }
                u32::from_ne_bytes(word)
            })
            .collect();
        Self {
            sid,
            files: Vec::new(),
            roots: Vec::new(),
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
        let _lock = mutation::Lock::acquire()?;
        self.roots.push(path.to_path_buf());
        visit_tree(path, |file| {
            if self.files.len() >= limit {
                return Err(entry_limit(path, limit));
            }
            handle::ensure_single_link(&file).map_err(|error| failure(path, &error.to_string()))?;
            mutation::update(&file, &self.sid, Some(access))?;
            self.files.push(file);
            Ok(())
        })
    }
}

impl Grants {
    pub(super) fn cleanup(&mut self) -> Result<(), SandboxError> {
        let _lock = mutation::Lock::acquire()?;
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
        if let Some(error) = failure {
            return Err(error);
        }
        self.files.clear();
        self.roots.clear();
        Ok(())
    }
}

impl Drop for Grants {
    fn drop(&mut self) {
        if !self.files.is_empty() || !self.roots.is_empty() {
            let _ = self.cleanup();
        }
    }
}

fn visit_tree(
    root: &Path,
    mut visit: impl FnMut(File) -> Result<(), SandboxError>,
) -> Result<(), SandboxError> {
    let mut pins = pin_ancestors(root)?;
    let mut pending = vec![(root.to_path_buf(), true)];
    let mut entries = 0;
    while let Some((path, is_root)) = pending.pop() {
        if entries >= MAX_ACL_ENTRIES {
            return Err(entry_limit(root, MAX_ACL_ENTRIES));
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
        visit(file)?;
        if directory {
            pins.push(pin);
            for entry in
                std::fs::read_dir(&path).map_err(|error| failure(&path, &error.to_string()))?
            {
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
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    match options
        .access_mode(FILE_READ_ATTRIBUTES | FILE_READ_DATA)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
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
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "regression tests assert permission boundaries"
    )]
    fn acl_setup_does_not_require_file_content_access() -> Result<(), Box<dyn std::error::Error>> {
        let identity = super::super::identity::Identity::create()?;
        let root = std::env::temp_dir().join(format!(
            "kraai-acl-unreadable-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root)?;
        let file = root.join("unreadable");
        std::fs::write(&file, b"content")?;
        let mut command = std::process::Command::new("icacls.exe");
        command.arg(&file).args(["/deny", "*S-1-1-0:(RD)"]);
        assert!(crate::spawn_command(&mut command)?.wait()?.success());
        assert_eq!(
            File::open(&file).err().map(|error| error.kind()),
            Some(std::io::ErrorKind::PermissionDenied)
        );
        let mut grants = Grants::new(identity.sid.bytes());
        for access in [Access::Read, Access::Write] {
            grants.grant(&root, access)?;
            assert_eq!(grants.files.len(), 2);
            grants.cleanup()?;
            assert!(File::open(&file).is_err());
        }
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "regression tests assert permission boundaries"
    )]
    fn hard_link_grants_are_rejected_before_mutation() -> Result<(), Box<dyn std::error::Error>> {
        let identity = super::super::identity::Identity::create()?;
        let root =
            std::env::temp_dir().join(format!("kraai-acl-links-{:032x}", rand::random::<u128>()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        let external = root.join("secret");
        let link = workspace.join("link");
        std::fs::write(&external, b"host data")?;
        std::fs::hard_link(&external, &link)?;
        let mut grants = Grants::new(identity.sid.bytes());
        for access in [Access::Read, Access::Write] {
            let result = grants.grant(&link, access);
            assert!(
                matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("multiple hard links"))
            );
            assert!(grants.files.is_empty());
            grants.cleanup()?;
            let result = grants.grant(&workspace, access);
            assert!(
                matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("multiple hard links"))
            );
            grants.cleanup()?;
        }
        std::fs::remove_file(link)?;
        grants.grant(&workspace, Access::Write)?;
        grants.cleanup()?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "regression tests assert permission boundaries"
    )]
    fn traversal_pins_prevent_ancestor_replacement() -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("kraai-acl-pins-{:032x}", rand::random::<u128>()));
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested)?;
        std::fs::write(nested.join("file"), b"content")?;
        let mut visited = 0;
        visit_tree(&root, |_| {
            visited += 1;
            assert!(std::fs::rename(&root, root.with_extension("moved")).is_err());
            Ok(())
        })?;
        assert_eq!(visited, 3);
        let moved = root.with_extension("moved");
        std::fs::rename(&root, &moved)?;
        std::fs::remove_dir_all(moved)?;
        Ok(())
    }

    #[test]
    fn retained_grants_allow_renaming_existing_directories()
    -> Result<(), Box<dyn std::error::Error>> {
        let identity = super::super::identity::Identity::create()?;
        let root =
            std::env::temp_dir().join(format!("kraai-acl-rename-{:032x}", rand::random::<u128>()));
        let original = root.join("original");
        let moved = root.join("moved");
        std::fs::create_dir_all(original.join("nested"))?;
        std::fs::write(original.join("nested/file"), b"content")?;
        let mut grants = Grants::new(identity.sid.bytes());
        grants.grant(&original, Access::Write)?;
        std::fs::rename(&original, &moved)?;
        std::fs::rename(moved.join("nested"), moved.join("renamed"))?;
        grants.cleanup()?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "regression tests assert observable behavior"
    )]
    fn entry_limit_preserves_cleanup_of_partial_grants() -> Result<(), Box<dyn std::error::Error>> {
        let identity = super::super::identity::Identity::create()?;
        let root =
            std::env::temp_dir().join(format!("kraai-acl-limit-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root)?;
        for name in ["one", "two", "three"] {
            std::fs::write(root.join(name), b"content")?;
        }
        let mut grants = Grants::new(identity.sid.bytes());
        let result = grants.grant_with_limit(&root, Access::Read, 2);
        assert!(
            matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("ACL entry limit (2) exceeded"))
        );
        assert_eq!(grants.files.len(), 2);
        grants.cleanup()?;
        assert!(grants.files.is_empty());
        grants.grant_with_limit(&root, Access::Read, 4)?;
        grants.cleanup()?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
