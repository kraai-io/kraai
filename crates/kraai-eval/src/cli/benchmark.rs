use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Args;
use color_eyre::eyre::{Context, Result, ensure};
use kraai_eval::{HarnessProfile, ProxyKind};

#[derive(Debug, Args)]
pub(super) struct BenchmarkArgs {
    #[command(flatten)]
    pricing: super::accounting::PricingArgs,
    dataset: String,
    #[arg(long, required_unless_present = "oracle")]
    model: Option<String>,
    #[arg(long, conflicts_with = "oracle")]
    harness: Option<PathBuf>,
    #[arg(long, conflicts_with = "oracle")]
    runner: Option<PathBuf>,
    #[arg(
        long,
        required_unless_present = "full_dataset",
        conflicts_with = "full_dataset"
    )]
    task_name: Vec<String>,
    #[arg(
        long,
        help = "Run the entire pinned dataset rather than selected task names"
    )]
    full_dataset: bool,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..))]
    attempts: u64,
    #[arg(long)]
    output_dir: Option<PathBuf>,
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
    if !args.oracle {
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
    let job_dir = args.output_dir.clone().unwrap_or_else(|| {
        PathBuf::from(".kraai-eval-cache/public")
            .join(if args.oracle { "oracle" } else { &profile.name })
            .join(ulid::Ulid::generate().to_string())
    });
    let job_dir = std::path::absolute(job_dir)?;
    ensure!(
        !job_dir.exists(),
        "job directory already exists; choose another --output-dir"
    );
    let project = project_directory()?;
    if args.dry_run {
        super::print_json(&serde_json::json!({
            "backend": "harbor==0.22.0", "dataset": dataset, "model": args.model,
            "harness": if args.oracle { "oracle" } else { &profile.name },
            "runner": args.runner.as_ref().unwrap_or(&profile.program),
            "tasks": args.task_name, "full_dataset": args.full_dataset,
            "attempts": args.attempts, "concurrency": 1, "retries": 0,
            "job_dir": job_dir, "proxy_host": args.proxy_host, "wall_clock_reliable": false,
        }))?;
        return Ok(ExitCode::SUCCESS);
    }
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
        !preparation.exists(),
        "benchmark input directory already exists: {}",
        preparation.display()
    );
    let mut command = Command::new("uv");
    command.env(
        "UV_PROJECT_ENVIRONMENT",
        std::path::absolute(".kraai-eval-cache/harbor-venv")?,
    );
    command
        .args(["run", "--locked", "--python", "python3.12", "--project"])
        .arg(project)
        .args([
            "python",
            "-m",
            "kraai_harbor.run",
            "--dataset",
            &dataset,
            "--job-dir",
        ])
        .arg(&job_dir)
        .args(["--attempts", &args.attempts.to_string()]);
    if args.oracle {
        command.arg("--oracle");
    } else {
        let harness = profile.resolve(
            args.model.as_deref().unwrap_or_default(),
            args.runner.as_deref(),
        )?;
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
        command
            .arg("--spec")
            .arg(spec)
            .args(["--allow-agent-host", &proxy_host]);
    }
    for name in args.task_name {
        command.args(["--task-name", &name]);
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
            .write(true)
            .create_new(true)
            .open(&stdout_path)?;
        command.stdout(stdout);
    }
    let status = command.status().wrap_err("launch Harbor; use the repository's nix develop environment for uv, and a running Docker engine with Compose")?;
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

fn dataset_version(value: &str) -> Result<String> {
    let dataset = match value {
        "terminal-bench" => "terminal-bench@2.0",
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
        ensure!(dataset_version("terminal-bench")? == "terminal-bench@2.0");
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
