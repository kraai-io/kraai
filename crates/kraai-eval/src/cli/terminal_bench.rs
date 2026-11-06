use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use color_eyre::eyre::{Context, Result, ensure};
use kraai_eval::{EvalRoot, ProxyKind};

use super::{Benchmark, Prepared, RunArgs, exit_code, print_json};

const DATASET: &str = "terminal-bench/terminal-bench@4.0.0";

pub(super) fn run(args: RunArgs, prepared: Prepared, json: bool) -> Result<ExitCode> {
    let Prepared {
        root,
        profile,
        harness,
        directory,
    } = prepared;
    ensure!(
        profile.has_model_proxy(),
        "terminal-bench needs a harness with a model proxy for usage accounting"
    );
    let Some(tasks) = args.tasks else {
        color_eyre::eyre::bail!("terminal-bench needs --tasks N");
    };
    let job_dir = directory.path.join("harbor");
    let project = project_directory()?;
    if args.dry_run {
        print_json(&serde_json::json!({
            "benchmark": args.benchmark,
            "dataset": DATASET,
            "tasks": args.tasks,
            "attempts": args.attempts,
            "harness": harness.name,
            "program": harness.program,
            "args": harness.args,
            "version": harness.version,
            "model": harness.model,
            "reasoning": harness.reasoning,
            "results": directory.path,
        }))?;
        return Ok(ExitCode::SUCCESS);
    }
    let _lock = super::lock(&root, &directory)?;
    kraai_eval::runner_store_root(&harness.program)?;
    let proxy_host = args
        .proxy_host
        .clone()
        .map_or_else(kraai_eval::docker_proxy_host, Ok)?;
    let mut proxy_command = vec![
        std::env::current_exe()?.to_string_lossy().into_owned(),
        String::from("proxy"),
        String::from("--listen"),
        String::from("0.0.0.0:0"),
        String::from("--advertise-host"),
        proxy_host.clone(),
        String::from("--max-requests"),
        profile.max_requests.to_string(),
    ];
    args.pricing.append(&mut proxy_command);
    if profile.proxy == ProxyKind::Openai {
        proxy_command.extend([
            String::from("--proxy"),
            String::from("openai"),
            String::from("--credential-env"),
            profile.api_key_env.clone(),
        ]);
    }
    if profile.sanitize_kraai_provider {
        proxy_command.extend([
            String::from("--provider-config"),
            super::provider_config_path(&args)?
                .canonicalize()?
                .to_string_lossy()
                .into_owned(),
        ]);
        if let Some(provider) = &args.provider {
            proxy_command.extend([String::from("--provider"), provider.clone()]);
        }
    }
    let spec = kraai_eval::prepare_benchmark_spec(
        &harness,
        proxy_command,
        &root.cache().join("runners"),
        &root.staging(&directory),
    )?;
    let mut command = harbor_command(&root, &project, &job_dir)?;
    command
        .args(["--attempts", &args.attempts.to_string(), "--spec"])
        .arg(spec)
        .args([
            "--model",
            harness.model.as_str(),
            "--allow-agent-host",
            &proxy_host,
        ]);
    command.args(["--task-count", &tasks.to_string()]);
    let stdout_path = directory.path.join("harbor.stdout.log");
    if json {
        std::fs::create_dir_all(&directory.path)?;
        command.stdout(
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&stdout_path)?,
        );
    }
    let status = command.status().wrap_err("launch Harbor; use the repository's nix develop environment for uv, and a running Docker engine with Compose");
    let summary_path = directory.path.join("summary.json");
    let summary = std::fs::read(&summary_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    if json {
        print_json(&serde_json::json!({
            "results": directory.path,
            "exit_code": status.as_ref().ok().and_then(std::process::ExitStatus::code),
            "stdout_log": stdout_path,
            "summary": summary,
            "error": status.as_ref().err().map(|error| format!("{error:#}")),
        }))?;
    } else if let Some(summary) = &summary {
        println!("{}", format_summary(summary, &directory.path));
    }
    Ok(exit_code(status?.success()))
}

pub(super) fn check(root: &EvalRoot, tasks: &[String]) -> Result<ExitCode> {
    ensure!(
        tasks
            .iter()
            .all(|name| !name.is_empty() && !name.contains(['*', '?', '['])),
        "task names must be exact names without glob patterns"
    );
    let project = project_directory()?;
    let job_dir = root
        .cache()
        .join("check")
        .join(Benchmark::TerminalBench.directory())
        .join("harbor");
    let mut command = harbor_command(root, &project, &job_dir)?;
    command.arg("--oracle");
    if tasks.is_empty() {
        command.arg("--full-dataset");
    }
    for name in tasks {
        command.args(["--task-name", name]);
    }
    let status = command.status().wrap_err("launch Harbor; use the repository's nix develop environment for uv, and a running Docker engine with Compose")?;
    Ok(exit_code(status.success()))
}

fn harbor_command(root: &EvalRoot, project: &Path, job_dir: &Path) -> Result<Command> {
    let relative = job_dir.strip_prefix(root.path()).unwrap_or(job_dir);
    let mut lock = root.cache().join("locks").join(relative).into_os_string();
    lock.push(".harbor.lock");
    let lock = PathBuf::from(lock);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut command = Command::new("uv");
    command.envs(kraai_eval::benchmark_environment());
    command.env("UV_PROJECT_ENVIRONMENT", root.cache().join("harbor-venv"));
    command
        .args(["run", "--locked", "--python", "python3.12", "--project"])
        .arg(project)
        .args(["python", "-m", "kraai_harbor.run", "--dataset", DATASET])
        .arg("--job-dir")
        .arg(job_dir)
        .arg("--lock-file")
        .arg(lock);
    Ok(command)
}

fn format_summary(summary: &serde_json::Value, results: &Path) -> String {
    let tasks = summary
        .get("tasks")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .map(|(_, counts)| counts);
    let mut totals = [0_u64; 4];
    for counts in tasks {
        for (total, key) in totals
            .iter_mut()
            .zip(["passed", "failed", "errors", "remaining"])
        {
            *total += counts
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
        }
    }
    let [passed, failed, errors, remaining] = totals;
    format!(
        "Attempts: {passed} passed, {failed} failed, {errors} errors, {remaining} remaining\nResults folder: {}",
        results.display()
    )
}

fn project_directory() -> Result<PathBuf> {
    let path = std::env::var_os("KRAAI_EVAL_HARBOR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("evals/harbor").to_owned());
    path.canonicalize().wrap_err(
        "Harbor integration is unavailable; run from the repository or set KRAAI_EVAL_HARBOR",
    )
}
