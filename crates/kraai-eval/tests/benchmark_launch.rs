use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{Result, ensure};

fn temporary(name: &str) -> Result<PathBuf> {
    let root = std::env::temp_dir().join(format!("kraai-{name}-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    Ok(root)
}

fn dry_run(eval: &Path, runner: &Path, extra: &[&str]) -> Result<serde_json::Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_kraai-eval"))
        .args([
            "run",
            "terminal-bench",
            "--model",
            "gpt-6-astra",
            "--dry-run",
            "--price-input",
            "10",
            "--price-output",
            "50",
        ])
        .arg("--eval-dir")
        .arg(eval)
        .arg("--runner")
        .arg(runner)
        .args(extra)
        .env("KRAAI_EVAL_HARBOR", std::env::temp_dir())
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn missing_uv_reports_a_harbor_launch_failure() -> Result<()> {
    let root = temporary("benchmark-launch")?;
    let output = Command::new(env!("CARGO_BIN_EXE_kraai-eval"))
        .args(["check", "terminal-bench", "test", "--eval-dir"])
        .arg(root.join("eval"))
        .env("PATH", &root)
        .env("KRAAI_EVAL_HARBOR", &root)
        .current_dir(&root)
        .output()?;
    ensure!(!output.status.success());
    ensure!(String::from_utf8_lossy(&output.stderr).contains("launch Harbor"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn more_tasks_and_attempts_reuse_the_same_results_folder() -> Result<()> {
    let root = temporary("benchmark-folder")?;
    let runner = std::env::current_exe()?;
    let mut folders = Vec::new();
    for (count, attempts) in [("5", "1"), ("10", "1"), ("10", "2")] {
        let plan = dry_run(
            &root,
            &runner,
            &[
                "--reasoning",
                "low",
                "--tasks",
                count,
                "--attempts",
                attempts,
            ],
        )?;
        ensure!(
            plan.get("dataset").and_then(serde_json::Value::as_str)
                == Some("terminal-bench/terminal-bench@4.0.0")
        );
        ensure!(plan.get("tasks").and_then(serde_json::Value::as_u64) == Some(count.parse()?));
        folders.push(plan.get("results").cloned());
    }
    ensure!(
        !root.join("prices.json").exists(),
        "a dry run saved a price"
    );
    ensure!(folders.iter().all(|folder| Some(folder) == folders.first()));
    let folder = folders
        .first()
        .and_then(|folder| folder.as_ref())
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();
    ensure!(
        folder.starts_with(
            root.canonicalize()?
                .join("terminal-bench-4.0.0/gpt-6-astra-low")
        )
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn terminal_bench_requires_a_task_count() -> Result<()> {
    let root = temporary("benchmark-count")?;
    ensure!(dry_run(&root, &std::env::current_exe()?, &[]).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn runner_changes_get_their_own_results_folder() -> Result<()> {
    let root = temporary("benchmark-version")?;
    let runner = root.join("runner");
    let mut folders = Vec::new();
    for contents in ["first version", "second version"] {
        fs::write(&runner, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&runner, fs::Permissions::from_mode(0o755))?;
        }
        let plan = dry_run(&root.join("eval"), &runner, &["--tasks", "5"])?;
        folders.push(plan.get("results").cloned());
    }
    ensure!(folders.first() != folders.last());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn terminal_bench_needs_a_proxy_and_provider_flags_need_a_kraai_profile() -> Result<()> {
    let root = temporary("benchmark-profile")?;
    let profile_path = root.join("harness.toml");
    let mut profile = kraai_eval::HarnessProfile::kraai();
    profile.sanitize_kraai_provider = false;
    profile.args.clear();
    for (proxy, provider_args, expected_error) in [
        (
            kraai_eval::ProxyKind::None,
            vec![],
            "needs a harness with a model proxy",
        ),
        (
            kraai_eval::ProxyKind::Openai,
            vec!["--provider", "test"],
            "require a harness profile with sanitize_kraai_provider",
        ),
        (
            kraai_eval::ProxyKind::Openai,
            vec!["--provider-config", "missing.toml"],
            "require a harness profile with sanitize_kraai_provider",
        ),
    ] {
        profile.proxy = proxy;
        fs::write(&profile_path, toml::to_string(&profile)?)?;
        let output = Command::new(env!("CARGO_BIN_EXE_kraai-eval"))
            .args([
                "run",
                "terminal-bench",
                "--dry-run",
                "--model",
                "test-model",
            ])
            .arg("--harness")
            .arg(&profile_path)
            .arg("--runner")
            .arg(std::env::current_exe()?)
            .arg("--eval-dir")
            .arg(root.join("eval"))
            .args(&provider_args)
            .env("KRAAI_EVAL_HARBOR", &root)
            .output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(!output.status.success());
        ensure!(stderr.contains(expected_error), "{stderr}");
    }
    fs::remove_dir_all(root)?;
    Ok(())
}
