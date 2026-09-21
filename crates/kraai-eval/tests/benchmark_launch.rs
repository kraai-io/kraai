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

#[test]
fn terminal_bench_selection_is_pinned_and_attempts_reuse_the_same_directory() -> Result<()> {
    let mut directories = Vec::new();
    for (count, attempts) in [("5", "1"), ("10", "1"), ("10", "2")] {
        let output = Command::new(env!("CARGO_BIN_EXE_kraai-eval"))
            .args([
                "benchmark",
                "terminal-bench",
                "--model",
                "gpt-6-astra-low",
                "--task-count",
                count,
                "--attempts",
                attempts,
                "--dry-run",
            ])
            .arg("--runner")
            .arg(std::env::current_exe()?)
            .env("KRAAI_EVAL_HARBOR", std::env::temp_dir())
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let plan: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        ensure!(
            plan.get("dataset").and_then(serde_json::Value::as_str)
                == Some("terminal-bench/terminal-bench@4.0.0")
        );
        ensure!(plan.get("task_count").and_then(serde_json::Value::as_u64) == Some(count.parse()?));
        directories.push(plan.get("job_dir").cloned());
    }
    ensure!(
        directories
            .iter()
            .all(|directory| Some(directory) == directories.first())
    );
    Ok(())
}

#[test]
fn task_count_cannot_be_combined_with_a_different_selection_mode() -> Result<()> {
    for selection in [vec!["--full-dataset"], vec!["--task-name", "one"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_kraai-eval"))
            .args([
                "benchmark",
                "terminal-bench",
                "--oracle",
                "--task-count",
                "2",
                "--dry-run",
            ])
            .args(selection)
            .output()?;
        ensure!(!output.status.success());
    }
    Ok(())
}

#[test]
fn status_requires_a_model_or_directory_except_for_oracle() -> Result<()> {
    for selection in [
        vec![],
        vec!["--oracle"],
        vec!["--output-dir", "saved-job"],
        vec!["--model", "test-model"],
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kraai-eval"));
        command
            .args(["benchmark", "terminal-bench", "--status", "--dry-run"])
            .args(&selection);
        if selection.contains(&"--model") {
            command.arg("--runner").arg(std::env::current_exe()?);
        }
        let output = command
            .env("KRAAI_EVAL_HARBOR", std::env::temp_dir())
            .output()?;
        if selection.is_empty() {
            ensure!(!output.status.success());
            ensure!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("--status requires --output-dir or --model")
            );
        } else {
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    Ok(())
}

#[test]
fn runner_changes_separate_cached_benchmark_versions() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "kraai-benchmark-version-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root)?;
    let runner = root.join("runner");
    let mut directories = Vec::new();
    for contents in ["first version", "second version"] {
        fs::write(&runner, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&runner, fs::Permissions::from_mode(0o755))?;
        }
        let output = Command::new(env!("CARGO_BIN_EXE_kraai-eval"))
            .args([
                "benchmark",
                "terminal-bench",
                "--model",
                "gpt-6-astra-low",
                "--task-count",
                "5",
                "--dry-run",
                "--runner",
            ])
            .arg(&runner)
            .env("KRAAI_EVAL_HARBOR", &root)
            .output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let plan: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        directories.push(plan.get("job_dir").cloned());
    }
    ensure!(directories.first() != directories.last());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn status_skips_execution_profile_checks_but_runs_preserve_them() -> Result<()> {
    let root =
        std::env::temp_dir().join(format!("kraai-status-profile-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    let profile_path = root.join("harness.toml");
    let mut profile = kraai_eval::HarnessProfile::kraai();
    profile.sanitize_kraai_provider = false;
    profile.args.clear();
    for (proxy, provider_args, expected_error) in [
        (kraai_eval::ProxyKind::None, vec![], "require a model proxy"),
        (
            kraai_eval::ProxyKind::Openai,
            vec!["--provider", "test"],
            "provider flags require a Kraai profile",
        ),
        (
            kraai_eval::ProxyKind::Openai,
            vec!["--provider-config", "missing.toml"],
            "provider flags require a Kraai profile",
        ),
    ] {
        profile.proxy = proxy;
        fs::write(&profile_path, toml::to_string(&profile)?)?;
        for status in [true, false] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_kraai-eval"));
            command
                .args(["benchmark", "terminal-bench", "--dry-run", "--harness"])
                .arg(&profile_path)
                .arg("--output-dir")
                .arg(root.join("saved-job"))
                .args(&provider_args)
                .env("KRAAI_EVAL_HARBOR", &root);
            if status {
                command.arg("--status");
            } else {
                command.args(["--model", "test-model", "--task-count", "1"]);
            }
            let output = command.output()?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            if status {
                ensure!(output.status.success(), "{stderr}");
            } else {
                ensure!(!output.status.success());
                ensure!(stderr.contains(expected_error), "{stderr}");
            }
        }
    }
    fs::remove_dir_all(root)?;
    Ok(())
}
