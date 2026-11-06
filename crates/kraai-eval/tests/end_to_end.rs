use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{Result, bail, ensure};
use kraai_eval::{RunRequest, RunStatus, SuiteRequest};

#[test]
fn cli_runs_resume_and_only_add_new_attempts() -> Result<()> {
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
    let task_dir = root.join("tasks/cli-profile");
    fs::create_dir_all(task_dir.join("fixture"))?;
    fs::write(task_dir.join("fixture/answer.txt"), "broken\n")?;
    fs::write(
        task_dir.join("task.toml"),
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
        program: shell.clone(),
        args: vec![
            String::from("-c"),
            String::from("printf 'fixed\\n' > answer.txt"),
        ],
        reasoning_args: Vec::new(),
        proxy: kraai_eval::ProxyKind::None,
        sanitize_kraai_provider: false,
        ..kraai_eval::HarnessProfile::kraai()
    };
    fs::write(&profile, toml::to_string(&harness)?)?;
    let eval = root.join("eval");
    let invoke = |attempts: &str| -> Result<kraai_eval::SuiteResult> {
        let output = Command::new(binary)
            .args(["run", "in-house", "--json", "--harness"])
            .arg(&profile)
            .arg("--eval-dir")
            .arg(&eval)
            .arg("--tasks-dir")
            .arg(root.join("tasks"))
            .args(["--model", " fixture/model ", "--attempts", attempts])
            .output()?;
        ensure!(
            output.status.success(),
            "CLI run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    };
    let first = invoke("2")?;
    ensure!(
        first.harness_name == "external-shell"
            && first.model_label.as_deref() == Some("fixture/model"),
        "suite labels were not normalized"
    );
    ensure!(
        first.passed_runs == 2,
        "external profile did not pass both attempts"
    );
    let version = kraai_eval::HarnessProfile::load(&profile)?
        .resolve("fixture/model", None, None)?
        .version;
    let date = chrono::Local::now().format("%Y-%m-%d");
    let folder = Path::new("in-house/fixture-model").join(format!(
        "{date}-external-shell-{}",
        version.get(..8).unwrap_or_default()
    ));
    ensure!(
        first.artifact_path == folder,
        "{}",
        first.artifact_path.display()
    );
    let first_attempt = eval.join(&folder).join("cli-profile/attempt-1/result.json");
    let saved = fs::read(&first_attempt)?;
    let resumed = invoke("2")?;
    ensure!(resumed.requested_runs == 2 && resumed.passed_runs == 2);
    ensure!(
        fs::read(&first_attempt)? == saved,
        "a saved attempt was run again"
    );
    let grown = invoke("3")?;
    ensure!(grown.requested_runs == 3 && grown.passed_runs == 3);
    ensure!(
        fs::read(&first_attempt)? == saved,
        "a saved attempt was run again"
    );
    ensure!(eval.join(&folder).join("summary.json").is_file());
    ensure!(
        !eval.join(".cache/staging").join(&folder).exists()
            || fs::read_dir(eval.join(".cache/staging").join(&folder))?.count() == 0,
        "staging kept a committed attempt"
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
timeout_seconds = {timeout}
network = "disabled"
rust_toolchain = true

[grader]
hidden_patch = "hidden.patch"

[[grader.commands]]
command = ["cargo", "test", "--locked", "--offline"]
timeout_seconds = {timeout}

[[grader.commands]]
command = ["{}", "-c", "read -r actual < answer.txt; read -r expected < hidden.expected; [ \"$actual\" = \"$expected\" ]"]
timeout_seconds = {timeout}
"#,
            repository.display(),
            revision,
            shell.display(),
            timeout = kraai_test_support::HANG_TIMEOUT.as_secs(),
        ),
    )?;

    let eval = root.join("eval");
    fs::create_dir_all(&eval)?;
    let eval = eval.canonicalize()?;
    let staging = eval.join(".cache/staging/fixture");
    let attempt =
        |number: u64| PathBuf::from("fixture/hidden-grader").join(format!("attempt-{number}"));
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
        attempt: 1,
        eval_root: eval.clone(),
        artifact_path: attempt(1),
        staging_dir: staging.clone(),
        model_proxy: None,
        kraai_provider_config: None,
        progress: None,
    };
    let result = kraai_eval::run(&request)?;
    let result_artifacts = eval.join(&result.artifact_path);
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

    ensure!(
        result.artifact_path == attempt(1),
        "result was not saved at the requested attempt path"
    );
    let object = eval.join(&result.artifact_path);
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

    let reused = kraai_eval::run(&request)?;
    ensure!(
        reused.experiment_id == result.experiment_id
            && reused.started_at_ms == result.started_at_ms,
        "a saved attempt was run again"
    );

    let failed = kraai_eval::run(&RunRequest {
        task_path: task_dir.join("task.toml"),
        runner_program: shell.clone(),
        runner_args: vec![String::from("-c"), String::from("exit 7")],
        runner_version: String::from("test-shell"),
        harness_name: Some(String::from("fixture-shell")),
        model_label: Some(String::from("deterministic-fixture")),
        attempt: 2,
        eval_root: eval.clone(),
        artifact_path: attempt(2),
        staging_dir: staging.clone(),
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
        attempt: 3,
        eval_root: eval.clone(),
        artifact_path: attempt(3),
        staging_dir: staging.clone(),
        model_proxy: None,
        kraai_provider_config: None,
        progress: None,
    })?;
    ensure!(
        controller_failed.status == RunStatus::ControllerFailed,
        "controller error was not preserved as a result"
    );
    let controller_artifact = eval.join(&controller_failed.artifact_path);
    ensure!(
        controller_artifact.join("result.json").is_file()
            && controller_artifact.join("controller-error.log").is_file(),
        "controller failure artifacts were not committed"
    );
    let retried = kraai_eval::run(&RunRequest {
        task_path: task_dir.join("task.toml"),
        runner_program: shell.clone(),
        runner_args: vec![String::from("{proxy_url}")],
        runner_version: String::from("controller-failure-test-shell"),
        harness_name: Some(String::from("fixture-shell")),
        model_label: Some(String::from("deterministic-fixture")),
        attempt: 3,
        eval_root: eval.clone(),
        artifact_path: attempt(3),
        staging_dir: staging.clone(),
        model_proxy: None,
        kraai_provider_config: None,
        progress: None,
    })?;
    let work = |result: &kraai_eval::RunResult| {
        result
            .controller_failure
            .as_ref()
            .map(|failure| failure.retained_work_path.clone())
    };
    ensure!(
        retried.status == RunStatus::ControllerFailed
            && work(&retried).is_some()
            && work(&retried) != work(&controller_failed),
        "a controller failure was reused instead of retried"
    );
    for failure in [&controller_failed, &retried].into_iter().filter_map(work) {
        fs::remove_dir_all(failure)?;
    }
    let suite = kraai_eval::run_suite(&SuiteRequest {
        runs: vec![request.clone()],
        eval_root: eval.clone(),
        harness_dir: PathBuf::from("fixture"),
    })?;
    ensure!(
        suite.requested_runs == 3
            && suite.evaluated_runs == 2
            && suite.passed_runs == 1
            && suite.controller_failures == 1
            && suite.success_rate == Some(0.5),
        "suite did not summarize every saved attempt"
    );
    ensure!(
        suite.total_tokens.total == 30 && eval.join("fixture/summary.json").is_file(),
        "suite metrics or summary artifact is missing"
    );
    let launch_error = kraai_eval::run(&RunRequest {
        task_path: task_dir.join("missing.toml"),
        runner_program: PathBuf::from("/bin/sh"),
        runner_args: Vec::new(),
        runner_version: String::from("launch-failure"),
        harness_name: Some(String::from("fixture-shell")),
        model_label: None,
        attempt: 4,
        eval_root: eval.clone(),
        artifact_path: attempt(4),
        staging_dir: staging,
        model_proxy: None,
        kraai_provider_config: None,
        progress: None,
    });
    ensure!(launch_error.is_err(), "invalid task unexpectedly launched");
    ensure!(
        !eval.join(attempt(4)).exists(),
        "a launch failure saved an attempt"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

fn run_git(repository: &Path, args: &[&str]) -> Result<()> {
    let status =
        kraai_sandbox::spawn_command(Command::new("git").arg("-C").arg(repository).args(args))?
            .wait()?;
    if !status.success() {
        bail!("git command failed: {}", args.join(" "));
    }
    Ok(())
}

fn git_output(repository: &Path, args: &[&str]) -> Result<String> {
    let output = kraai_sandbox::spawn_command(
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )?
    .wait_with_output()?;
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
