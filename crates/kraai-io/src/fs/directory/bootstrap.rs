use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{create_directories_with_sync, sync_directory};

/// The anchor must already be durable. The first successful initialization syncs
/// every descendant through it, including directories left by failed attempts.
#[derive(Debug)]
pub struct DirectoryBootstrap {
    anchor: PathBuf,
    path: PathBuf,
    ready: Mutex<bool>,
}

impl DirectoryBootstrap {
    pub fn new(anchor: PathBuf, path: PathBuf) -> Self {
        Self {
            anchor,
            path,
            ready: Mutex::new(false),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_private(&self) -> io::Result<()> {
        self.create_private_with_sync(sync_directory)
    }

    fn create_private_with_sync(
        &self,
        sync: impl FnMut(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut ready = self
            .ready
            .lock()
            .map_err(|_error| io::Error::other("Directory bootstrap lock poisoned"))?;
        if *ready && self.path.is_dir() {
            return Ok(());
        }
        *ready = false;
        create_directories_with_sync(&self.anchor, &self.path, true, sync)?;
        *ready = true;
        drop(ready);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
