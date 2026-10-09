use std::fs;
use std::path::Path;
use std::process::Command;

use color_eyre::eyre::{Result, ensure};

pub(super) fn render_command(folded: &Path, title: &str, count_name: &str) -> Command {
    let mut command = Command::new("inferno-flamegraph");
    command
        .args([
            "--deterministic",
            "--title",
            title,
            "--countname",
            count_name,
        ])
        .arg(folded);
    command
}

pub(super) fn validate_svg(path: &Path) -> Result<()> {
    let svg = fs::read_to_string(path)?;
    ensure!(
        svg.contains("<svg") && svg.contains("</svg>"),
        "profiler did not produce an SVG flame graph"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn publish(staging: tempfile::TempDir, output: &Path) -> Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staging.path(),
        rustix::fs::CWD,
        output,
        rustix::fs::RenameFlags::NOREPLACE,
    )?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn publish(_staging: tempfile::TempDir, _output: &Path) -> Result<()> {
    color_eyre::eyre::bail!("profiling requires Linux")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn publishing_does_not_replace_an_existing_directory() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let output = parent.path().join("profile");
        fs::create_dir(&output)?;
        let staging = tempfile::tempdir_in(parent.path())?;
        fs::write(staging.path().join("profile.json"), b"new profile")?;
        let result = publish(staging, &output);
        ensure!(result.is_err());
        ensure!(output.read_dir()?.next().is_none());
        Ok(())
    }
}
