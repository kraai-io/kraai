use std::process::{Command, Stdio};

use color_eyre::eyre::{Result, ensure, eyre};

pub fn fixture_command(module: &str, name: &str) -> Result<Command> {
    let (_, module) = module
        .split_once("::")
        .ok_or_else(|| eyre!("test fixture has no module path"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            &format!("{module}::{name}"),
            "--ignored",
            "--test-threads=1",
        ])
        .stdin(Stdio::null());
    Ok(command)
}

pub fn run_fixture(module: &str, name: &str) -> Result<()> {
    let output = fixture_command(module, name)?.output()?;
    ensure!(
        output.status.success(),
        "test fixture {name} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    ensure!(
        String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
        "test fixture {name} did not execute exactly one test"
    );
    Ok(())
}
