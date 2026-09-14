use std::fs;
use std::process::Command;

use color_eyre::eyre::{Result, ensure};

#[test]
fn missing_uv_emits_json_launch_failure() -> Result<()> {
    let root =
        std::env::temp_dir().join(format!("kraai-benchmark-launch-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    let job = root.join("job");
    let output = Command::new(env!("CARGO_BIN_EXE_kraai-eval"))
        .args([
            "--json",
            "benchmark",
            "terminal-bench",
            "--oracle",
            "--task-name",
            "test",
            "--output-dir",
        ])
        .arg(&job)
        .env("PATH", &root)
        .env("KRAAI_EVAL_HARBOR", &root)
        .current_dir(&root)
        .output()?;
    ensure!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    ensure!(result.get("job_dir") == Some(&serde_json::to_value(&job)?));
    ensure!(
        result
            .get("exit_code")
            .is_some_and(serde_json::Value::is_null)
    );
    ensure!(result.get("stdout_log") == Some(&serde_json::to_value(root.join("job.stdout.log"))?));
    ensure!(
        result
            .get("error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|error| error.contains("launch Harbor"))
    );
    ensure!(String::from_utf8_lossy(&output.stderr).contains("launch Harbor"));
    fs::remove_dir_all(root)?;
    Ok(())
}
