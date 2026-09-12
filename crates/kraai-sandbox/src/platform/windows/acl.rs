#![expect(
    unsafe_code,
    reason = "Windows DACL edits use pinned file handles and OS-allocated security descriptors"
)]

mod mutation;

use std::fs::{File, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
    WRITE_DAC,
};

use crate::SandboxError;

#[derive(Clone, Copy)]
pub(super) enum Access {
    Read,
    Write,
    DenyWrite,
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
        let _lock = mutation::Lock::acquire()?;
        self.roots.push(path.to_path_buf());
        visit_tree(path, |file| {
            mutation::update(&file, &self.sid, Some(access))?;
            self.files.push(file);
            Ok(())
        })
    }
}

impl Drop for Grants {
    fn drop(&mut self) {
        let Ok(_lock) = mutation::Lock::acquire() else {
            return;
        };
        for file in &self.files {
            let _ = mutation::update(file, &self.sid, None);
        }
        for root in &self.roots {
            let _ = visit_tree(root, |file| mutation::update(&file, &self.sid, None));
        }
    }
}

fn visit_tree(
    root: &Path,
    mut visit: impl FnMut(File) -> Result<(), SandboxError>,
) -> Result<(), SandboxError> {
    let mut pins = pin_ancestors(root)?;
    let mut pending = vec![(root.to_path_buf(), true)];
    while let Some((path, is_root)) = pending.pop() {
        let pin = open(&path, FILE_READ_ATTRIBUTES, false)?;
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
        let file = open(&path, READ_CONTROL | WRITE_DAC, true)?;
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
        let pin = open(ancestor, FILE_READ_ATTRIBUTES, false)?;
        if is_reparse(&pin)? {
            return Err(failure(ancestor, "reparse point in sandbox root ancestry"));
        }
        pins.push(pin);
    }
    Ok(pins)
}

fn open(path: &Path, access: u32, share_mutations: bool) -> Result<File, SandboxError> {
    OpenOptions::new()
        .access_mode(access | FILE_READ_ATTRIBUTES)
        .share_mode(if share_mutations {
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        } else {
            FILE_SHARE_READ
        })
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| failure(path, &error.to_string()))
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
