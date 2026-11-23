// Clippy only applies its allow-*-in-tests settings to helpers under a `cfg(test)` ancestor.
#![cfg(test)]
#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{Result, ensure};
use kraai_eval::{RunRequest, RunStatus, SuiteRequest};
use support::{find_program, temporary_directory, user_scope_available};

mod support;

#[test]
fn cli_runs_resume_and_only_add_new_attempts() {
    let binary = env!("CARGO_BIN_EXE_kraai-eval");
    let Some(shell) = find_program("sh") else {
        return;
    };
    if find_program("bwrap").is_none() || !user_scope_available() {
        return;
    }
    let root = temporary_directory("cli-profile");
    let task_dir = root.join("tasks/cli-profile");
    fs::create_dir_all(task_dir.join("fixture")).unwrap();
    fs::write(task_dir.join("fixture/answer.txt"), "broken\n").unwrap();
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
    )
    .unwrap();
    let profile = root.join("agent.toml");
    let harness = kraai_eval::HarnessProfile {
        name: String::from(" external-shell "),
        program: shell,
        args: vec![
            String::from("-c"),
            String::from("printf 'fixed\\n' > answer.txt"),
        ],
        reasoning_args: Vec::new(),
        proxy: kraai_eval::ProxyKind::None,
        sanitize_kraai_provider: false,
        ..kraai_eval::HarnessProfile::kraai()
    };
    fs::write(&profile, toml::to_string(&harness).unwrap()).unwrap();
    let eval = root.join("eval");
    let command = |model: &str, attempts: &str, extra: &[&str]| -> Result<std::process::Output> {
        Ok(Command::new(binary)
            .args(["run", "in-house", "--json", "--harness"])
            .arg(&profile)
            .arg("--eval-dir")
            .arg(&eval)
            .arg("--tasks-dir")
            .arg(root.join("tasks"))
            .args(["--model", model, "--attempts", attempts])
            .args(extra)
            .output()?)
    };
    let invoke_with = |attempts: &str, extra: &[&str]| -> Result<kraai_eval::SuiteResult> {
        let output = command(" fixture/model ", attempts, extra)?;
        ensure!(
            output.status.success(),
            "CLI run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    };
    let invoke = |attempts: &str| invoke_with(attempts, &[]);
    let unpriced = command("fixture/unpriced", "1", &[]).unwrap();
    assert!(
        !unpriced.status.success()
            && String::from_utf8_lossy(&unpriced.stderr).contains("has no saved price")
            && !eval.join("in-house/fixture-unpriced").exists(),
        "a model without a saved price was run"
    );
    let first = invoke_with("2", &["--price-input", "1", "--price-output", "2"]).unwrap();
    assert!(
        kraai_eval::EvalRoot::open(&eval)
            .unwrap()
            .model_price("fixture/model")
            .unwrap()
            == Some(kraai_eval::ModelPrice {
                input: 1.0,
                output: 2.0,
                cache_read: None,
                cache_write: None,
            }),
        "the price was not saved under the model id"
    );
    assert!(
        first.harness_name == "external-shell"
            && first.model_label.as_deref() == Some("fixture/model"),
        "suite labels were not normalized"
    );
    assert!(
        first.passed_runs == 2,
        "external profile did not pass both attempts"
    );
    let version = kraai_eval::HarnessProfile::load(&profile)
        .unwrap()
        .resolve("fixture/model", None, None)
        .unwrap()
        .version;
    let date = chrono::Local::now().format("%Y-%m-%d");
    let folder = Path::new("in-house/fixture-model").join(format!(
        "{date}-external-shell-{}",
        version.get(..8).unwrap_or_default()
    ));
    assert!(
        first.artifact_path == folder,
        "{}",
        first.artifact_path.display()
    );
    let first_attempt = eval.join(&folder).join("cli-profile/attempt-1/result.json");
    let saved = fs::read(&first_attempt).unwrap();
    let resumed = invoke("2").unwrap();
    assert!(resumed.requested_runs == 2 && resumed.passed_runs == 2);
    assert!(
        fs::read(&first_attempt).unwrap() == saved,
        "a saved attempt was run again"
    );
    let grown = invoke("3").unwrap();
    assert!(grown.requested_runs == 3 && grown.passed_runs == 3);
    assert!(
        fs::read(&first_attempt).unwrap() == saved,
        "a saved attempt was run again"
    );
    assert!(eval.join(&folder).join("summary.json").is_file());
    assert!(
        !eval.join(".cache/staging").join(&folder).exists()
            || fs::read_dir(eval.join(".cache/staging").join(&folder))
                .unwrap()
                .count()
                == 0,
        "staging kept a committed attempt"
    );
    fs::write(
        eval.join(&folder).join("cli-profile/attempt-3/result.json"),
        "{}",
    )
    .unwrap();
    for attempts in ["2", "3"] {
        let output = command(" fixture/model ", attempts, &[]).unwrap();
        let summary: kraai_eval::SuiteResult = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            summary.requested_runs == 3 && summary.passed_runs == 2 && summary.launch_failures == 1,
            "an unreadable attempt with {attempts} requested attempts was not counted once"
        );
    }
    let summary_path = eval.join(&folder).join("summary.json");
    let summary = fs::read(&summary_path).unwrap();
    let colliding = command(
        "fixture-model",
        "4",
        &["--price-input", "1", "--price-output", "2"],
    )
    .unwrap();
    assert!(
        !colliding.status.success()
            && String::from_utf8_lossy(&colliding.stderr).contains("same results folder"),
        "a model that maps to the same folder was run in it"
    );
    assert!(
        !eval.join(&folder).join("cli-profile/attempt-4").exists()
            && fs::read(&summary_path).unwrap() == summary,
        "a model that maps to the same folder changed its results"
    );
    assert!(
        fs::read(&first_attempt).unwrap() == saved,
        "a saved attempt was run again"
    );
}

#[test]
fn agent_cannot_see_hidden_test_and_submission_is_graded_from_clean_base() {
    let Some(shell) = find_program("sh") else {
        return;
    };
    if find_program("bwrap").is_none() {
        return;
    }
    if !user_scope_available() {
        return;
    }

    let root = temporary_directory("hidden-grader");
    let repository = root.join("source");
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(repository.join("answer.txt"), "broken\n").unwrap();
    fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname = \"eval-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        repository.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"eval-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(repository.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    run_git(&repository, &["init", "--quiet"]);
    run_git(&repository, &["config", "user.name", "eval-test"]);
    run_git(&repository, &["config", "user.email", "eval-test@invalid"]);
    run_git(&repository, &["add", "."]);
    run_git(&repository, &["commit", "--quiet", "-m", "base"]);
    let revision = git_output(&repository, &["rev-parse", "HEAD"]);

    let task_dir = root.join("task");
    fs::create_dir_all(&task_dir).unwrap();
    fs::write(
        task_dir.join("hidden.patch"),
        "diff --git a/hidden.expected b/hidden.expected\nnew file mode 100644\n--- /dev/null\n+++ b/hidden.expected\n@@ -0,0 +1 @@\n+fixed\n",
    ).unwrap();
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
    ).unwrap();

    let eval = root.join("eval");
    fs::create_dir_all(&eval).unwrap();
    let eval = eval.canonicalize().unwrap();
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
        proxy_bridge: None,
        kraai_provider_config: None,
        progress: None,
    };
    let result = kraai_eval::run(&request).unwrap();
    let result_artifacts = eval.join(&result.artifact_path);
    let runner_stderr = fs::read_to_string(result_artifacts.join("runner.stderr.log"))
        .unwrap_or_else(|error| format!("<unavailable: {error}>"));
    assert!(
        result.status == RunStatus::Passed,
        "evaluation did not pass: status={:?}, runner={:?}, graders={:?}, stderr={runner_stderr:?}, artifacts={}",
        result.status,
        result.runner,
        result.graders,
        result_artifacts.display()
    );
    assert!(
        result
            .submission_sha256
            .as_ref()
            .is_some_and(|digest| !digest.is_empty()),
        "submission digest is empty"
    );
    assert!(
        result.started_at_ms <= result.completed_at_ms,
        "invalid timestamps"
    );
    assert!(
        result
            .metrics
            .harness
            .as_ref()
            .and_then(|metrics| metrics.turns)
            == Some(1),
        "harness metrics were not captured"
    );

    assert!(
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
        assert!(
            object.join(artifact).is_file(),
            "missing artifact {artifact}"
        );
    }
    assert!(
        object.join("script-executions").is_dir(),
        "missing script execution artifact directory"
    );

    let reused = kraai_eval::run(&request).unwrap();
    assert!(
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
        proxy_bridge: None,
        kraai_provider_config: None,
        progress: None,
    })
    .unwrap();
    assert!(
        result.status == RunStatus::Passed,
        "successful evaluation status changed"
    );
    assert!(
        failed.status == RunStatus::RunnerFailed,
        "failing runner status was not preserved"
    );
    assert!(
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
        proxy_bridge: None,
        kraai_provider_config: None,
        progress: None,
    })
    .unwrap();
    assert!(
        controller_failed.status == RunStatus::ControllerFailed,
        "controller error was not preserved as a result"
    );
    let controller_artifact = eval.join(&controller_failed.artifact_path);
    assert!(
        controller_artifact.join("result.json").is_file()
            && controller_artifact.join("controller-error.log").is_file(),
        "controller failure artifacts were not committed"
    );
    let retried = kraai_eval::run(&RunRequest {
        task_path: task_dir.join("task.toml"),
        runner_program: shell,
        runner_args: vec![String::from("{proxy_url}")],
        runner_version: String::from("controller-failure-test-shell"),
        harness_name: Some(String::from("fixture-shell")),
        model_label: Some(String::from("deterministic-fixture")),
        attempt: 3,
        eval_root: eval.clone(),
        artifact_path: attempt(3),
        staging_dir: staging.clone(),
        model_proxy: None,
        proxy_bridge: None,
        kraai_provider_config: None,
        progress: None,
    })
    .unwrap();
    let work = |result: &kraai_eval::RunResult| {
        result
            .controller_failure
            .as_ref()
            .map(|failure| failure.retained_work_path.clone())
    };
    assert!(
        retried.status == RunStatus::ControllerFailed
            && work(&retried).is_some()
            && work(&retried) != work(&controller_failed),
        "a controller failure was reused instead of retried"
    );
    for failure in [&controller_failed, &retried].into_iter().filter_map(work) {
        fs::remove_dir_all(failure).unwrap();
    }
    let suite = kraai_eval::run_suite(&SuiteRequest {
        runs: vec![request],
        eval_root: eval.clone(),
        harness_dir: PathBuf::from("fixture"),
    })
    .unwrap()
    .summary;
    assert!(
        suite.requested_runs == 3
            && suite.evaluated_runs == 2
            && suite.passed_runs == 1
            && suite.controller_failures == 1
            && suite.success_rate == Some(0.5),
        "suite did not summarize every saved attempt"
    );
    assert!(
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
        proxy_bridge: None,
        kraai_provider_config: None,
        progress: None,
    });
    assert!(launch_error.is_err(), "invalid task unexpectedly launched");
    assert!(
        !eval.join(attempt(4)).exists(),
        "a launch failure saved an attempt"
    );
}

fn run_git(repository: &Path, args: &[&str]) {
    let status =
        kraai_sandbox::spawn_command(Command::new("git").arg("-C").arg(repository).args(args))
            .unwrap()
            .wait()
            .unwrap();
    if !status.success() {
        panic!("git command failed: {}", args.join(" "));
    }
}

fn git_output(repository: &Path, args: &[&str]) -> String {
    let output = kraai_sandbox::spawn_command(
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap()
    .wait_with_output()
    .unwrap();
    if !output.status.success() {
        panic!("git command failed: {}", args.join(" "));
    }
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
