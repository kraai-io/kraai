use std::fs;
use std::path::{Path, PathBuf};

pub(crate) struct TemporaryDirectory {
    path: PathBuf,
    cleanup: bool,
}

impl TemporaryDirectory {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            cleanup: true,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn keep(mut self) {
        self.cleanup = false;
    }

    pub(crate) fn remove(mut self) -> std::io::Result<()> {
        self.cleanup = false;
        fs::remove_dir_all(&self.path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
