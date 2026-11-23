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

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
