use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::Result;

pub fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

pub fn user_scope_available() -> bool {
    let Some(systemd_run) = find_program("systemd-run") else {
        return false;
    };
    let Some(true_program) = find_program("true") else {
        return false;
    };

    Command::new(systemd_run)
        .args(["--user", "--scope", "--quiet"])
        .arg(true_program)
        .output()
        .is_ok_and(|output| output.status.success())
}

pub struct TemporaryDirectory(PathBuf);

impl std::ops::Deref for TemporaryDirectory {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn temporary_directory(name: &str) -> Result<TemporaryDirectory> {
    let path = std::env::temp_dir().join(format!("kraai-eval-{name}-{}", ulid::Ulid::generate()));
    fs::create_dir(&path)?;
    Ok(TemporaryDirectory(path))
}
