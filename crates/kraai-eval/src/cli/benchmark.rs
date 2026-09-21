use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Args;
use color_eyre::eyre::{Context, Result, ensure};
use kraai_eval::{HarnessProfile, ProxyKind};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Args, Serialize)]
pub(super) struct BenchmarkArgs {
    #[command(flatten)]
    pricing: super::accounting::PricingArgs,
    dataset: String,
    #[arg(long, required_unless_present_any = ["oracle", "status"])]
    model: Option<String>,
    #[arg(long, conflicts_with = "oracle")]
    harness: Option<PathBuf>,
    #[arg(long, conflicts_with = "oracle")]
    runner: Option<PathBuf>,
    #[arg(
        long,
        required_unless_present_any = ["full_dataset", "task_count", "status"],
        conflicts_with = "full_dataset"
    )]
    task_name: Vec<String>,
    #[arg(
        long,
        help = "Run the entire pinned dataset rather than selected task names"
    )]
    full_dataset: bool,
    #[arg(long, conflicts_with_all = ["task_name", "full_dataset"], value_parser = clap::value_parser!(u64).range(1..))]
    task_count: Option<u64>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..))]
    attempts: u64,
    #[arg(long)]
    output_dir: Option<PathBuf>,
    #[arg(
        long,
        help = "Read completed and remaining attempts without running tasks"
    )]
    status: bool,
    #[arg(long)]
    registry_path: Option<PathBuf>,
    #[arg(long, conflicts_with = "oracle")]
    provider: Option<String>,
    #[arg(long, conflicts_with = "oracle")]
    provider_config: Option<PathBuf>,
    #[arg(
        long,
        help = "Controller IP reachable from benchmark containers; defaults to the local Docker bridge gateway"
    )]
    proxy_host: Option<String>,
    #[arg(
        long,
        help = "Validate official reference solutions without model calls"
    )]
    oracle: bool,
    #[arg(
        long,
        help = "Print the selected dataset, harness and execution plan without installing or running anything"
    )]
    dry_run: bool,
}

pub(super) fn execute(args: BenchmarkArgs, json: bool) -> Result<ExitCode> {
    let dataset = dataset_version(&args.dataset)?;
    ensure!(
        !args.status || args.output_dir.is_some() || args.model.is_some() || args.oracle,
        "--status requires --output-dir or --model unless --oracle is used"
    );
    ensure!(
        args.task_name
            .iter()
            .all(|name| !name.is_empty() && !name.contains(['*', '?', '['])),
        "task names must be exact names without glob patterns"
    );
    let unique = args
        .task_name
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    ensure!(unique.len() == args.task_name.len(), "duplicate task names");
    let profile = args
        .harness
        .as_deref()
        .map(HarnessProfile::load)
        .transpose()?
        .unwrap_or_else(HarnessProfile::kraai);
    if !args.oracle && !args.status {
        ensure!(
            profile.proxy != ProxyKind::None,
            "public harness comparisons require a model proxy for usage accounting"
        );
        ensure!(
            profile.sanitize_kraai_provider
                || (args.provider.is_none() && args.provider_config.is_none()),
            "provider flags require a Kraai profile"
        );
    }
    let resolved = if args.oracle || (args.status && args.output_dir.is_some()) {
        None
    } else {
        Some(profile.resolve(
            args.model.as_deref().unwrap_or_default(),
            args.runner.as_deref(),
        )?)
    };
    let mut request = serde_json::to_value(&args)?;
    if let Some(object) = request.as_object_mut() {
        for key in [
            "attempts",
            "output_dir",
            "dry_run",
            "status",
            "task_name",
            "task_count",
            "full_dataset",
        ] {
            object.remove(key);
        }
        object.insert("dataset".into(), serde_json::to_value(&dataset)?);
        object.insert("profile".into(), serde_json::to_value(&profile)?);
        if let Some(harness) = &resolved {
            object.insert("runner".into(), serde_json::to_value(&harness.program)?);
            object.insert(
                "runner_version".into(),
                serde_json::to_value(&harness.version)?,
            );
            object.insert(
                "runner_sha256".into(),
                serde_json::to_value(kraai_eval::hash_file(&harness.program)?)?,
            );
        }
    }
    request.sort_all_objects();
    let digest = Sha256::digest(serde_json::to_vec(&request)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let job_dir = std::path::absolute(args.output_dir.clone().unwrap_or_else(|| {
        PathBuf::from(".kraai-eval-cache/public")
            .join(kraai_eval::path_segment(&dataset, "dataset"))
            .join(kraai_eval::path_segment(&profile.name, "harness"))
            .join(kraai_eval::path_segment(
                resolved.as_ref().map_or("oracle", |h| h.version.as_str()),
                "version",
            ))
            .join(kraai_eval::path_segment(
                args.model.as_deref().unwrap_or("oracle"),
                "model",
            ))
            .join(digest)
    }))?;
    let project = project_directory()?;
    if args.dry_run {
        super::print_json(&serde_json::json!({
            "backend": "harbor==0.22.0", "dataset": dataset, "model": args.model,
            "harness": if args.oracle { "oracle" } else { &profile.name },
            "runner": resolved.as_ref().map(|h| &h.program),
            "runner_version": resolved.as_ref().map(|h| &h.version),
            "tasks": args.task_name, "full_dataset": args.full_dataset, "task_count": args.task_count,
            "attempts": args.attempts, "concurrency": 1, "retries": 0,
            "job_dir": job_dir, "proxy_host": args.proxy_host, "wall_clock_reliable": false,
        }))?;
        return Ok(ExitCode::SUCCESS);
    }
    if args.status {
        let status = harbor_command(&project)?
            .args(["--dataset", &dataset, "--status", "--job-dir"])
            .arg(&job_dir)
            .status()?;
        return Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    if let Some(parent) = job_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let preparation_lock = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(job_dir.with_extension("prepare.lock"))?;
    preparation_lock
        .try_lock()
        .wrap_err("benchmark is already running")?;
    let saved_spec = job_dir.join("kraai-eval-spec.json");
    let proxy_host = if args.oracle {
        String::new()
    } else {
        args.proxy_host
            .map_or_else(kraai_eval::docker_proxy_host, Ok)?
    };
    let preparation = job_dir.with_file_name(format!(
        "{}.inputs",
        job_dir.file_name().unwrap_or_default().to_string_lossy()
    ));
    ensure!(
        !job_dir.exists() || job_dir.join("kraai-run.json").exists(),
        "existing job has no resumable Kraai manifest; choose a new --output-dir"
    );
    let request_path = preparation.join("request.json");
    if request_path.exists() {
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&request_path)?)?;
        ensure!(
            saved == request,
            "benchmark configuration changed; choose a new --output-dir"
        );
    } else {
        std::fs::create_dir_all(&preparation)?;
        std::fs::write(&request_path, serde_json::to_vec_pretty(&request)?)?;
    }
    let mut command = harbor_command(&project)?;
    command
        .args(["--dataset", &dataset, "--job-dir"])
        .arg(&job_dir)
        .args(["--attempts", &args.attempts.to_string()]);
    if let Some(path) = &args.registry_path {
        command.arg("--registry-path").arg(path.canonicalize()?);
    }
    if args.oracle {
        command.arg("--oracle");
    } else if saved_spec.exists() {
        command.arg("--spec").arg(&saved_spec).args([
            "--model",
            args.model.as_deref().unwrap_or_default(),
            "--allow-agent-host",
            &proxy_host,
        ]);
    } else {
        let harness = resolved.ok_or_else(|| color_eyre::eyre::eyre!("missing resolved runner"))?;
        kraai_eval::runner_store_root(&harness.program)?;
        let mut proxy_command = vec![
            std::env::current_exe()?.to_string_lossy().into_owned(),
            String::from("proxy"),
            String::from("--listen"),
            String::from("0.0.0.0:0"),
            String::from("--advertise-host"),
            proxy_host.clone(),
            String::from("--max-requests"),
            profile.max_requests.to_string(),
            String::from("--proxy"),
            String::from(if profile.proxy == ProxyKind::Openai {
                "openai"
            } else {
                "codex_subscription"
            }),
        ];
        args.pricing.append(&mut proxy_command)?;
        if profile.proxy == ProxyKind::Openai {
            proxy_command.extend([String::from("--credential-env"), profile.api_key_env]);
        }
        if profile.sanitize_kraai_provider {
            let source = args.provider_config.map_or_else(
                || kraai_persistence::agent_state_root().map(|root| root.join("providers.toml")),
                Ok,
            )?;
            proxy_command.extend([
                String::from("--provider-config"),
                source.canonicalize()?.to_string_lossy().into_owned(),
            ]);
            if let Some(provider) = args.provider {
                proxy_command.extend([String::from("--provider"), provider]);
            }
        }
        let spec = kraai_eval::prepare_benchmark_spec(harness, proxy_command, &preparation)?;
        command.arg("--spec").arg(spec).args([
            "--model",
            args.model.as_deref().unwrap_or_default(),
            "--allow-agent-host",
            &proxy_host,
        ]);
    }
    for name in args.task_name {
        command.args(["--task-name", &name]);
    }
    if let Some(count) = args.task_count {
        command.args(["--task-count", &count.to_string()]);
    }
    if args.full_dataset {
        command.arg("--full-dataset");
    }
    let mut stdout_name = job_dir.file_name().unwrap_or_default().to_os_string();
    stdout_name.push(".stdout.log");
    let stdout_path = job_dir.with_file_name(stdout_name);
    if json {
        if let Some(parent) = stdout_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let stdout = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&stdout_path)?;
        command.stdout(stdout);
    }
    let status = command.status().wrap_err("launch Harbor; use the repository's nix develop environment for uv, and a running Docker engine with Compose");
    if let Err(error) = &status
        && json
    {
        super::print_json(&serde_json::json!({
            "job_dir": job_dir, "exit_code": null, "stdout_log": stdout_path,
            "error": format!("{error:#}"),
        }))?;
    }
    let status = status?;
    if json {
        super::print_json(
            &serde_json::json!({"job_dir": job_dir, "exit_code": status.code(), "stdout_log": stdout_path}),
        )?;
    }
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn harbor_command(project: &Path) -> Result<Command> {
    let mut command = Command::new("uv");
    command.envs(kraai_eval::benchmark_environment());
    command.env(
        "UV_PROJECT_ENVIRONMENT",
        std::path::absolute(".kraai-eval-cache/harbor-venv")?,
    );
    command
        .args(["run", "--locked", "--python", "python3.12", "--project"])
        .arg(project)
        .args(["python", "-m", "kraai_harbor.run"]);
    Ok(command)
}

fn dataset_version(value: &str) -> Result<String> {
    let dataset = match value {
        "terminal-bench" => "terminal-bench/terminal-bench@4.0.0",
        "swe-bench" => "swebench-verified@1.0",
        value => value,
    };
    let (name, version) = dataset.rsplit_once('@').ok_or_else(|| {
        color_eyre::eyre::eyre!("specify a pinned DATASET@VERSION or terminal-bench / swe-bench")
    })?;
    ensure!(
        !name.is_empty() && !version.is_empty() && !matches!(version, "latest" | "head" | "main"),
        "dataset version must be fixed"
    );
    Ok(dataset.to_owned())
}

fn project_directory() -> Result<PathBuf> {
    let path = std::env::var_os("KRAAI_EVAL_HARBOR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("evals/harbor").to_owned());
    path.canonicalize().wrap_err(
        "Harbor integration is unavailable; run from the repository or set KRAAI_EVAL_HARBOR",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_suites_require_a_pinned_dataset() -> Result<()> {
        ensure!(dataset_version("terminal-bench")? == "terminal-bench/terminal-bench@4.0.0");
        ensure!(dataset_version("swe-bench")? == "swebench-verified@1.0");
        for invalid in [
            "other",
            "other@",
            "@1",
            "other@latest",
            "other@head",
            "other@main",
        ] {
            ensure!(dataset_version(invalid).is_err());
        }
        Ok(())
    }
}
