use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{Result, bail, ensure};
use kraai_eval::{RunRequest, RunStatus, SuiteRequest};

#[test]
fn cli_runs_an_external_profile_resumes_and_compares_attempts() -> Result<()> {
    let Some(binary) = option_env!("CARGO_BIN_EXE_kraai-eval") else {
        return Ok(());
    };
    let Some(shell) = find_program("sh") else {
        return Ok(());
    };
    if find_program("bwrap").is_none() || !user_scope_available() {
        return Ok(());
    }
    let root = temporary_directory("cli-profile")?;
    fs::create_dir(root.join("fixture"))?;
    fs::write(root.join("fixture/answer.txt"), "broken\n")?;
    let task = root.join("task.toml");
    fs::write(
        &task,
        r#"schema_version = 1
id = "cli-profile"
prompt = "Repair answer.txt."
[source]
directory = "fixture"
[[grader.commands]]
command = ["sh", "-c", "read -r actual < answer.txt; [ \"$actual\" = fixed ]"]
"#,
    )?;
    let profile = root.join("agent.toml");
    let harness = kraai_eval::HarnessProfile {
        name: String::from(" external-shell "),
        program: shell,
        args: vec![
            String::from("-c"),
            String::from("printf 'fixed\\n' > answer.txt"),
        ],
        version: Some(String::from("fixture-v1")),
        proxy: kraai_eval::ProxyKind::None,
        sanitize_kraai_provider: false,
        ..kraai_eval::HarnessProfile::kraai()
    };
    fs::write(&profile, toml::to_string(&harness)?)?;
    let cache = root.join("cache");
    let invoke = |extra: &[&str]| {
        Command::new(binary)
            .arg("run")
            .arg(&task)
            .args(["--harness"])
            .arg(&profile)
            .args(["--cache-dir"])
            .arg(&cache)
            .args(["--model", " fixture-model ", "--attempts", "2", "--json"])
            .args(extra)
            .output()
    };
    let first = invoke(&[])?;
    ensure!(
        first.status.success(),
        "CLI run failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: kraai_eval::SuiteResult = serde_json::from_slice(&first.stdout)?;
    ensure!(
        first.harness_name == "external-shell"
            && first.model_label.as_deref() == Some("fixture-model"),
        "suite labels were not normalized"
    );
    ensure!(
        first.passed_runs == 2,
        "external profile did not pass both attempts"
    );
    let duplicate = invoke(&[])?;
    ensure!(
        !duplicate.status.success(),
        "duplicate attempts unexpectedly succeeded"
    );
    let duplicate: kraai_eval::SuiteResult = serde_json::from_slice(&duplicate.stdout)?;
    ensure!(
        duplicate.launch_failures == 2,
        "duplicate results were silently reused"
    );
    let resumed = invoke(&["--resume"])?;
    ensure!(
        resumed.status.success(),
        "CLI resume failed: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed: kraai_eval::SuiteResult = serde_json::from_slice(&resumed.stdout)?;
    let comparison = Command::new(binary)
        .arg("compare")
        .arg(cache.join(first.artifact_path).join("summary.json"))
        .arg(cache.join(resumed.artifact_path).join("summary.json"))
        .arg("--json")
        .output()?;
    ensure!(
        comparison.status.success(),
        "CLI comparison failed: {}",
        String::from_utf8_lossy(&comparison.stderr)
    );
    let comparison: kraai_eval::ComparisonResult = serde_json::from_slice(&comparison.stdout)?;
    ensure!(
        comparison.evaluated_pairs == 2 && comparison.ties == 2,
        "cached attempts did not pair correctly"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn agent_cannot_see_hidden_test_and_submission_is_graded_from_clean_base() -> Result<()> {
    let Some(shell) = find_program("sh") else {
        return Ok(());
    };
    if find_program("bwrap").is_none() {
        return Ok(());
    }
    if !user_scope_available() {
        return Ok(());
    }

    let root = temporary_directory("hidden-grader")?;
    let repository = root.join("source");
    fs::create_dir_all(repository.join("src"))?;
    fs::write(repository.join("answer.txt"), "broken\n")?;
    fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname = \"eval-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(
        repository.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"eval-fixture\"\nversion = \"0.1.0\"\n",
    )?;
    fs::write(repository.join("src/lib.rs"), "pub fn fixture() {}\n")?;
    run_git(&repository, &["init", "--quiet"])?;
    run_git(&repository, &["config", "user.name", "eval-test"])?;
    run_git(&repository, &["config", "user.email", "eval-test@invalid"])?;
    run_git(&repository, &["add", "."])?;
    run_git(&repository, &["commit", "--quiet", "-m", "base"])?;
    let revision = git_output(&repository, &["rev-parse", "HEAD"])?;

    let task_dir = root.join("task");
    fs::create_dir_all(&task_dir)?;
    fs::write(
        task_dir.join("hidden.patch"),
        "diff --git a/hidden.expected b/hidden.expected\nnew file mode 100644\n--- /dev/null\n+++ b/hidden.expected\n@@ -0,0 +1 @@\n+fixed\n",
    )?;
    fs::write(
        task_dir.join("task.toml"),
        format!(
            r#"schema_version = 1
id = "hidden-grader"
prompt = "Repair answer.txt. Write any useful tests you need."
max_submission_bytes = 1048576

[source]
repository = "{}"
revision = "{}"

[runner]
timeout_seconds = 10
network = "disabled"
rust_toolchain = true

[grader]
hidden_patch = "hidden.patch"

[[grader.commands]]
command = ["cargo", "test", "--locked", "--offline"]
timeout_seconds = 10

[[grader.commands]]
command = ["{}", "-c", "read -r actual < answer.txt; read -r expected < hidden.expected; [ \"$actual\" = \"$expected\" ]"]
timeout_seconds = 10
"#,
            repository.display(),
            revision,
            shell.display(),
        ),
    )?;

    let cache = root.join("cache");
    let request = RunRequest {
        task_path: task_dir.join("task.toml"),
        runner_program: shell.clone(),
        runner_args: vec![
            String::from("-c"),
            String::from(
                "test ! -e hidden.expected; printf 'fixed\\n' > answer.txt; printf '%s' '{\"schema_version\":1,\"turns\":1,\"script_executions\":2,\"final_context_tokens\":30,\"usage\":{\"total_tokens\":30,\"input_tokens\":20,\"output_tokens\":10,\"reasoning_tokens\":0,\"cache_read_tokens\":0}}' > \"$KRAAI_EVAL_METRICS_PATH\"",
            ),
        ],
        runner_version: String::from("test-shell"),
        harness_name: Some(String::from("fixture-shell")),
        model_label: Some(String::from("deterministic-fixture")),
        attempt: 0,
        cache_dir: cache.clone(),
        reuse_result: false,
        model_proxy: None,
        kraai_provider_config: None,
        progress: None,
    };
    let result = kraai_eval::run(&request)?;
    let result_artifacts = cache.join(&result.artifact_path);
    let runner_stderr = fs::read_to_string(result_artifacts.join("runner.stderr.log"))
        .unwrap_or_else(|error| format!("<unavailable: {error}>"));
    ensure!(
        result.status == RunStatus::Passed,
        "evaluation did not pass: status={:?}, runner={:?}, graders={:?}, stderr={runner_stderr:?}, artifacts={}",
        result.status,
        result.runner,
        result.graders,
        result_artifacts.display()
    );
    ensure!(
        result
            .submission_sha256
            .as_ref()
            .is_some_and(|digest| !digest.is_empty()),
        "submission digest is empty"
    );
    ensure!(
        result.started_at_ms <= result.completed_at_ms,
        "invalid timestamps"
    );
    ensure!(
        result
            .metrics
            .harness
            .as_ref()
            .and_then(|metrics| metrics.turns)
            == Some(1),
        "harness metrics were not captured"
    );

    let expected_prefix = PathBuf::from("runs")
        .join("hidden-grader")
        .join("fixture-shell")
        .join("test-shell")
        .join("deterministic-fixture")
        .join("attempt-0");
    ensure!(
        result.artifact_path.starts_with(&expected_prefix),
        "result path is not grouped by useful run coordinates"
    );
    let object = cache.join(&result.artifact_path);
    for artifact in [
        "submission.patch",
        "events.jsonl",
        "runner.stdout.log",
        "grader-0.stderr.log",
    ] {
        ensure!(
            object.join(artifact).is_file(),
            "missing artifact {artifact}"
        );
    }
    ensure!(
        object.join("script-executions").is_dir(),
        "missing script execution artifact directory"
    );

    let reused = kraai_eval::run(&RunRequest {
        reuse_result: true,
        ..request
    })?;
    ensure!(
        reused.experiment_id == result.experiment_id,
        "cached experiment identity changed"
    );

    let failed = kraai_eval::run(&RunRequest {
        task_path: task_dir.join("task.toml"),
        runner_program: shell.clone(),
        runner_args: vec![String::from("-c"), String::from("exit 7")],
        runner_version: String::from("test-shell"),
        harness_name: Some(String::from("fixture-shell")),
        model_label: Some(String::from("deterministic-fixture")),
        attempt: 0,
        cache_dir: cache.clone(),
        reuse_result: false,
        model_proxy: None,
        kraai_provider_config: None,
        progress: None,
    })?;
    ensure!(
        result.status == RunStatus::Passed,
        "successful evaluation status changed"
    );
    ensure!(
        failed.status == RunStatus::RunnerFailed,
        "failing runner status was not preserved"
    );
    ensure!(
        failed.graders.is_empty(),
        "graders ran after the runner failed"
    );
    let controller_failed = kraai_eval::run(&RunRequest {
        task_path: task_dir.join("task.toml"),
        runner_program: shell.clone(),
        runner_args: vec![String::from("{proxy_url}")],
        runner_version: String::from("controller-failure-test-shell"),
        harness_name: Some(String::from("fixture-shell")),
        model_label: Some(String::from("deterministic-fixture")),
        attempt: 0,
        cache_dir: cache.clone(),
        reuse_result: false,
        model_proxy: None,
        kraai_provider_config: None,
        progress: None,
    })?;
    ensure!(
        controller_failed.status == RunStatus::ControllerFailed,
        "controller error was not preserved as a result"
    );
    let controller_artifact = cache.join(&controller_failed.artifact_path);
    ensure!(
        controller_artifact.join("result.json").is_file()
            && controller_artifact.join("controller-error.log").is_file(),
        "controller failure artifacts were not committed"
    );
    if let Some(failure) = controller_failed.controller_failure {
        fs::remove_dir_all(failure.retained_work_path)?;
    }
    let suite = kraai_eval::run_suite(&SuiteRequest {
        runs: vec![
            RunRequest {
                task_path: task_dir.join("task.toml"),
                runner_program: shell.clone(),
                runner_args: vec![
                    String::from("-c"),
                    String::from(
                        "test ! -e hidden.expected; printf 'fixed\\n' > answer.txt; printf '%s' '{\"schema_version\":1,\"turns\":1,\"script_executions\":2,\"final_context_tokens\":30,\"usage\":{\"total_tokens\":30,\"input_tokens\":20,\"output_tokens\":10,\"reasoning_tokens\":0,\"cache_read_tokens\":0}}' > \"$KRAAI_EVAL_METRICS_PATH\"",
                    ),
                ],
                runner_version: String::from("test-shell"),
                harness_name: Some(String::from("fixture-shell")),
                model_label: Some(String::from("deterministic-fixture")),
                attempt: 0,
                cache_dir: cache.clone(),
                reuse_result: true,
                model_proxy: None,
                kraai_provider_config: None,
                progress: None,
            },
            RunRequest {
                task_path: task_dir.join("task.toml"),
                runner_program: shell,
                runner_args: vec![String::from("-c"), String::from("exit 7")],
                runner_version: String::from("test-shell"),
                harness_name: Some(String::from("fixture-shell")),
                model_label: Some(String::from("deterministic-fixture")),
                attempt: 0,
                cache_dir: cache.clone(),
                reuse_result: true,
                model_proxy: None,
                kraai_provider_config: None,
                progress: None,
            },
        ],
        output_root: cache.clone(),
    })?;
    ensure!(
        suite.evaluated_runs == 2 && suite.passed_runs == 1 && suite.success_rate == Some(0.5),
        "suite did not aggregate cached outcomes"
    );
    ensure!(
        suite.total_tokens.total == 30
            && cache
                .join(&suite.artifact_path)
                .join("summary.json")
                .is_file(),
        "suite metrics or summary artifact is missing"
    );
    let launch_error = kraai_eval::run(&RunRequest {
        task_path: task_dir.join("missing.toml"),
        runner_program: PathBuf::from("/bin/sh"),
        runner_args: Vec::new(),
        runner_version: String::from("launch-failure"),
        harness_name: Some(String::from("fixture-shell")),
        model_label: None,
        attempt: 0,
        cache_dir: cache.clone(),
        reuse_result: false,
        model_proxy: None,
        kraai_provider_config: None,
        progress: None,
    });
    ensure!(launch_error.is_err(), "invalid task unexpectedly launched");
    ensure!(
        fs::read_dir(cache.join("failures"))?.count() == 1,
        "launch failure artifact was not recorded"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

fn run_git(repository: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .status()?;
    if !status.success() {
        bail!("git command failed: {}", args.join(" "));
    }
    Ok(())
}

fn git_output(repository: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()?;
    if !output.status.success() {
        bail!("git command failed: {}", args.join(" "));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn user_scope_available() -> bool {
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

fn temporary_directory(name: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("kraai-eval-{name}-{}", ulid::Ulid::generate()));
    fs::create_dir(&path)?;
    Ok(path)
}
