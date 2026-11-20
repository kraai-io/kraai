use std::fs;
use std::path::PathBuf;
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

pub fn temporary_directory(name: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("kraai-eval-{name}-{}", ulid::Ulid::generate()));
    fs::create_dir(&path)?;
    Ok(path)
}
