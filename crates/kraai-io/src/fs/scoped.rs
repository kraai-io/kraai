use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

mod error;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;

#[cfg(windows)]
mod windows;

pub use error::ScopedReadError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymlinkPolicy {
    /// Allows confined symlinks on Linux. Other platforms reject symlinks.
    WithinRoot,
    Reject,
}

pub struct ScopedDirectory {
    root: PathBuf,
    directory: File,
    policy: SymlinkPolicy,
}

impl ScopedDirectory {
    pub fn open(root: &Path, policy: SymlinkPolicy) -> Result<Self, ScopedReadError> {
        let root: PathBuf = root.components().collect();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let directory = unix::open_root(&root)?;
        #[cfg(windows)]
        let directory = windows::open_root(&root)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        let directory = unsupported(&root)?;
        Ok(Self {
            root,
            directory,
            policy,
        })
    }

    pub fn open_file(&self, path: &Path) -> Result<File, ScopedReadError> {
        let relative = relative_path(&self.root, path)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return unix::open_file(&self.directory, relative, path, self.policy);
        #[cfg(windows)]
        return windows::open_file(&self.directory, relative, path, self.policy);
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = (relative, &self.directory, self.policy);
            unsupported(path)
        }
    }
}

fn relative_path<'a>(root: &Path, path: &'a Path) -> Result<&'a Path, ScopedReadError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_error| ScopedReadError::OutsideRoot(path.to_path_buf()))?;
    if relative.as_os_str().is_empty() {
        return Err(ScopedReadError::NotFile(path.to_path_buf()));
    }
    Ok(relative)
}

pub fn open_scoped_file(root: &Path, path: &Path) -> Result<File, ScopedReadError> {
    relative_path(root, path)?;
    ScopedDirectory::open(root, SymlinkPolicy::WithinRoot)?.open_file(path)
}

pub fn read_scoped_text_file(root: &Path, path: &Path) -> Result<String, ScopedReadError> {
    let mut file = open_scoped_file(root, path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|source| ScopedReadError::ReadText {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(contents)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_file(file: File, path: &Path) -> Result<File, ScopedReadError> {
    let metadata = file.metadata().map_err(|source| ScopedReadError::Inspect {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ScopedReadError::NotFile(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn unsupported<T>(path: &Path) -> Result<T, ScopedReadError> {
    Err(ScopedReadError::UnsupportedPlatform(path.to_path_buf()))
}

#[cfg(test)]
mod tests;
