use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::SandboxError;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct PrivateTempDir {
    path: PathBuf,
}

impl PrivateTempDir {
    pub(crate) fn create(base_dir: Option<&Path>) -> Result<Self, SandboxError> {
        let base_dir = base_dir.map_or_else(std::env::temp_dir, Path::to_path_buf);
        for _ in 0..64 {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = base_dir.join(format!("kraai-sandbox-{}-{id}", std::process::id()));
            match create_private_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(SandboxError::PrivateTemp(format!(
                        "unable to create '{}': {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(SandboxError::PrivateTemp(String::from(
            "unable to allocate a unique directory",
        )))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn apply_environment(&self, environment: &mut BTreeMap<OsString, OsString>) {
        let value = self.path.as_os_str().to_os_string();
        for name in ["TMPDIR", "TMP", "TEMP"] {
            environment.insert(OsString::from(name), value.clone());
        }
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
