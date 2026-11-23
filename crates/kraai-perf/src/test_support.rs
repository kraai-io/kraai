use std::process::{Command, Stdio};

pub fn fixture_command(module: &str, name: &str) -> Command {
    let (_, module) = module
        .split_once("::")
        .expect("test fixture has no module path");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("{module}::{name}"),
            "--ignored",
            "--test-threads=1",
        ])
        .stdin(Stdio::null());
    command
}

pub fn run_fixture(module: &str, name: &str) {
    let output = fixture_command(module, name).output().unwrap();
    assert!(
        output.status.success(),
        "test fixture {name} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
        "test fixture {name} did not execute exactly one test"
    );
}
