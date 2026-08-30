use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
use std::time::Duration;

use color_eyre::eyre::{Result, bail};

use crate::NetworkPolicy;
use crate::command::{CommandOutcome, run_trusted};

pub(crate) struct SandboxRequest {
    pub command: Vec<String>,
    pub workspace: PathBuf,
    pub timeout: Duration,
    pub network: NetworkPolicy,
    pub environment: BTreeMap<String, String>,
    pub extra_programs: Vec<PathBuf>,
    pub cargo_home: Option<PathBuf>,
    pub metrics_output: Option<PathBuf>,
    pub script_executions_dir: Option<PathBuf>,
    pub resource_limits: Option<ResourceLimits>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResourceLimits {
    pub max_memory_bytes: u64,
    pub max_processes: u64,
    pub cpu_quota_percent: u64,
}

pub(crate) struct RustEnvironment {
    pub cargo: PathBuf,
    pub programs: Vec<PathBuf>,
}

impl RustEnvironment {
    pub fn program_identity(&self) -> Result<Vec<String>> {
        let mut programs = self
            .programs
            .iter()
            .map(|program| {
                program
                    .canonicalize()
                    .map(|path| path.to_string_lossy().into_owned())
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        programs.sort();
        programs.dedup();
        Ok(programs)
    }
}

pub(crate) fn rust_environment() -> Result<RustEnvironment> {
    let cargo = find_program("cargo").ok_or_else(|| {
        color_eyre::eyre::eyre!("task requires a Rust toolchain, but cargo was not found on PATH")
    })?;
    let required_program = |name: &str| {
        find_program(name).ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "task requires a Rust development environment, but {name} was not found on PATH"
            )
        })
    };
    let mut programs = vec![
        cargo.clone(),
        required_program("cc")?,
        required_program("git")?,
        required_program("rg")?,
        required_program("sed")?,
        required_program("ls")?,
    ];
    if let Some(pkg_config) = find_program("pkg-config") {
        programs.push(pkg_config);
    }
    Ok(RustEnvironment { cargo, programs })
}

pub(crate) fn run_sandboxed(request: SandboxRequest) -> Result<CommandOutcome> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = request;
        bail!("the evaluation sandbox is currently only supported on Linux");
    }
    #[cfg(target_os = "linux")]
    {
        let bwrap = find_program("bwrap").ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "bubblewrap is required; refusing to run evaluation unsandboxed"
            )
        })?;
        let args = build_args(&request)?;
        let mut command = if let Some(limits) = &request.resource_limits {
            let systemd_run = find_program("systemd-run").ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "systemd-run is required to enforce evaluation cgroup limits"
                )
            })?;
            vec![
                systemd_run.to_string_lossy().into_owned(),
                String::from("--user"),
                String::from("--scope"),
                String::from("--quiet"),
                String::from("--collect"),
                String::from("--property"),
                format!("MemoryMax={}", limits.max_memory_bytes),
                String::from("--property"),
                format!("TasksMax={}", limits.max_processes),
                String::from("--property"),
                format!("CPUQuota={}%", limits.cpu_quota_percent),
                bwrap.to_string_lossy().into_owned(),
            ]
        } else {
            vec![bwrap.to_string_lossy().into_owned()]
        };
        command.extend(args);
        let mut outcome = run_trusted(&command, &request.workspace, request.timeout)?;
        outcome.command = request.command;
        Ok(outcome)
    }
}

#[cfg(target_os = "linux")]
fn build_args(request: &SandboxRequest) -> Result<Vec<String>> {
    if request.command.is_empty() {
        bail!("sandbox command must not be empty");
    }
    let workspace = request.workspace.canonicalize()?;
    let program = request
        .command
        .first()
        .ok_or_else(|| color_eyre::eyre::eyre!("sandbox command must not be empty"))?;
    let runner = if Path::new(program).components().count() > 1 {
        PathBuf::from(program)
    } else {
        find_program(program).ok_or_else(|| {
            color_eyre::eyre::eyre!("sandbox program not found on PATH: {program}")
        })?
    }
    .canonicalize()?;
    let mut args = vec![
        "--new-session",
        "--die-with-parent",
        "--unshare-user",
        "--unshare-pid",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/home",
        "--dir",
        "/home/eval",
        "--dir",
        "/home/eval/.cargo",
        "--dir",
        "/home/eval/.kraai",
        "--dir",
        "/home/eval/.kraai/data",
        "--dir",
        "/runner",
        "--dir",
        "/run",
        "--dir",
        "/run/kraai-eval",
        "--dir",
        "/nix",
        "--dir",
        "/nix/store",
        "--clearenv",
        "--setenv",
        "HOME",
        "/home/eval",
        "--setenv",
        "TMPDIR",
        "/tmp",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    let mut executable_roots = vec![runner.clone()];
    for program in &request.extra_programs {
        executable_roots.push(program.canonicalize()?);
    }
    let mut path_entries = executable_roots
        .iter()
        .filter_map(|program| program.parent())
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    path_entries.extend([String::from("/usr/bin"), String::from("/bin")]);
    let runner_name = runner
        .file_name()
        .ok_or_else(|| color_eyre::eyre::eyre!("sandbox program has no file name"))?
        .to_string_lossy()
        .into_owned();
    let mounted_runner = format!("/runner/{runner_name}");
    args.extend([
        String::from("--setenv"),
        String::from("PATH"),
        path_entries.join(":"),
    ]);
    if let Some(cargo_home) = &request.cargo_home {
        let cargo_home = cargo_home.canonicalize()?;
        for directory in ["registry", "git"] {
            let source = cargo_home.join(directory);
            if source.is_dir() {
                args.extend([
                    String::from("--ro-bind"),
                    source.to_string_lossy().into_owned(),
                    format!("/home/eval/.cargo/{directory}"),
                ]);
            }
        }
        args.extend([
            String::from("--setenv"),
            String::from("CARGO_HOME"),
            String::from("/home/eval/.cargo"),
            String::from("--setenv"),
            String::from("CARGO_NET_OFFLINE"),
            String::from("true"),
        ]);
    }
    for (name, value) in &request.environment {
        if name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0') {
            bail!("sandbox environment contains an invalid name or value");
        }
        args.extend([String::from("--setenv"), name.clone(), value.clone()]);
    }
    if let Some(metrics_output) = &request.metrics_output {
        let metrics_output = metrics_output.canonicalize()?;
        let sandbox_metrics_path = "/run/kraai-eval/harness-metrics.json";
        args.extend([
            String::from("--bind"),
            metrics_output.to_string_lossy().into_owned(),
            String::from(sandbox_metrics_path),
            String::from("--setenv"),
            String::from("KRAAI_EVAL_METRICS_PATH"),
            String::from(sandbox_metrics_path),
        ]);
    }
    if let Some(script_executions_dir) = &request.script_executions_dir {
        let script_executions_dir = script_executions_dir.canonicalize()?;
        args.extend([
            String::from("--bind"),
            script_executions_dir.to_string_lossy().into_owned(),
            String::from("/home/eval/.kraai/data/executions"),
        ]);
    }
    for root in ["/usr", "/bin", "/lib", "/lib64", "/etc/ssl/certs"] {
        if Path::new(root).exists() {
            args.extend([
                String::from("--ro-bind"),
                String::from(root),
                String::from(root),
            ]);
        }
    }
    if let Some(ca_bundle) = host_ca_bundle() {
        let sandbox_ca_bundle = "/run/kraai-eval/ca-bundle.crt";
        args.extend([
            String::from("--ro-bind"),
            ca_bundle.to_string_lossy().into_owned(),
            String::from(sandbox_ca_bundle),
            String::from("--setenv"),
            String::from("SSL_CERT_FILE"),
            String::from(sandbox_ca_bundle),
            String::from("--setenv"),
            String::from("NIX_SSL_CERT_FILE"),
            String::from(sandbox_ca_bundle),
        ]);
    }
    let mut closure_roots = Vec::new();
    for program in &executable_roots {
        closure_roots.extend(nix_runtime_closure(program)?);
    }
    closure_roots.sort();
    closure_roots.dedup();
    for root in closure_roots {
        args.extend([
            String::from("--ro-bind"),
            root.to_string_lossy().into_owned(),
            root.to_string_lossy().into_owned(),
        ]);
    }
    args.extend([
        String::from("--bind"),
        workspace.to_string_lossy().into_owned(),
        String::from("/workspace"),
        String::from("--ro-bind"),
        workspace.join(".git").to_string_lossy().into_owned(),
        String::from("/workspace/.git"),
        String::from("--ro-bind"),
        runner.to_string_lossy().into_owned(),
        mounted_runner.clone(),
    ]);
    if request.network == NetworkPolicy::Disabled {
        args.push(String::from("--unshare-net"));
    } else {
        for file in ["/etc/resolv.conf", "/etc/hosts", "/etc/nsswitch.conf"] {
            if Path::new(file).exists() {
                args.extend([
                    String::from("--ro-bind"),
                    String::from(file),
                    String::from(file),
                ]);
            }
        }
    }
    args.extend([
        String::from("--chdir"),
        String::from("/workspace"),
        String::from("--"),
    ]);
    args.push(mounted_runner);
    args.extend(
        request
            .command
            .iter()
            .skip(1)
            .map(|arg| arg.replace(&workspace.to_string_lossy().to_string(), "/workspace")),
    );
    Ok(args)
}

#[cfg(target_os = "linux")]
fn nix_runtime_closure(program: &Path) -> Result<Vec<PathBuf>> {
    if !program.starts_with("/nix/store") {
        return Ok(Vec::new());
    }
    let nix_store = find_program("nix-store").ok_or_else(|| {
        color_eyre::eyre::eyre!("nix-store is required to isolate a Nix runner closure")
    })?;
    let output = Command::new(nix_store)
        .args(["--query", "--requisites"])
        .arg(program)
        .output()?;
    if !output.status.success() {
        bail!(
            "failed to resolve runner Nix closure: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut roots = String::from_utf8(output.stdout)?
        .lines()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if roots.is_empty()
        || roots
            .iter()
            .any(|root| !root.starts_with("/nix/store") || !root.exists())
    {
        bail!("nix-store returned an invalid runner closure");
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

#[cfg(target_os = "linux")]
fn host_ca_bundle() -> Option<PathBuf> {
    std::env::var_os("SSL_CERT_FILE")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            PathBuf::from("/etc/ssl/certs/ca-certificates.crt"),
            PathBuf::from("/etc/ssl/certs/ca-bundle.crt"),
        ])
        .find_map(|candidate| candidate.canonicalize().ok().filter(|path| path.is_file()))
}

#[cfg(target_os = "linux")]
fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use color_eyre::eyre::ensure;
    use std::fs;

    #[test]
    fn sandbox_clears_host_environment_and_passes_only_explicit_values() -> Result<()> {
        let Some(shell) = find_program("sh") else {
            return Ok(());
        };
        if find_program("bwrap").is_none() {
            return Ok(());
        }
        let workspace =
            std::env::temp_dir().join(format!("kraai-eval-sandbox-env-{}", ulid::Ulid::generate()));
        fs::create_dir_all(workspace.join(".git"))?;
        let outcome = run_sandboxed(SandboxRequest {
            command: vec![
                shell.to_string_lossy().into_owned(),
                String::from("-c"),
                String::from(
                    "[ \"$EVAL_PROXY_TOKEN\" = short-lived ] && [ -z \"${CARGO_HOME:-}\" ]",
                ),
            ],
            workspace: workspace.clone(),
            timeout: Duration::from_secs(5),
            network: NetworkPolicy::Disabled,
            environment: BTreeMap::from([(
                String::from("EVAL_PROXY_TOKEN"),
                String::from("short-lived"),
            )]),
            extra_programs: Vec::new(),
            cargo_home: None,
            metrics_output: None,
            script_executions_dir: None,
            resource_limits: None,
        })?;
        ensure!(outcome.success(), "sandbox did not isolate environment");
        ensure!(
            !outcome.command.iter().any(|item| item == "short-lived"),
            "sandbox credential leaked into recorded command"
        );
        fs::remove_dir_all(workspace)?;
        Ok(())
    }

    #[test]
    fn sandbox_mounts_a_readable_resolved_ca_bundle() -> Result<()> {
        let Some(shell) = find_program("sh") else {
            return Ok(());
        };
        if find_program("bwrap").is_none() || host_ca_bundle().is_none() {
            return Ok(());
        }
        let workspace =
            std::env::temp_dir().join(format!("kraai-eval-sandbox-ca-{}", ulid::Ulid::generate()));
        fs::create_dir_all(workspace.join(".git"))?;
        let outcome = run_sandboxed(SandboxRequest {
            command: vec![
                shell.to_string_lossy().into_owned(),
                String::from("-c"),
                String::from(
                    "[ \"$SSL_CERT_FILE\" = /run/kraai-eval/ca-bundle.crt ] && [ \"$NIX_SSL_CERT_FILE\" = \"$SSL_CERT_FILE\" ] && [ -r \"$SSL_CERT_FILE\" ]",
                ),
            ],
            workspace: workspace.clone(),
            timeout: Duration::from_secs(5),
            network: NetworkPolicy::Disabled,
            environment: BTreeMap::new(),
            extra_programs: Vec::new(),
            cargo_home: None,
            metrics_output: None,
            script_executions_dir: None,
            resource_limits: None,
        })?;
        ensure!(outcome.success(), "sandbox CA bundle was not readable");
        fs::remove_dir_all(workspace)?;
        Ok(())
    }

    #[test]
    fn sandbox_persists_script_execution_artifacts() -> Result<()> {
        let Some(shell) = find_program("sh") else {
            return Ok(());
        };
        if find_program("bwrap").is_none() {
            return Ok(());
        }
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-script-output-{}",
            ulid::Ulid::generate()
        ));
        let workspace = root.join("workspace");
        let executions = root.join("script-executions");
        fs::create_dir_all(workspace.join(".git"))?;
        fs::create_dir_all(&executions)?;
        let outcome = run_sandboxed(SandboxRequest {
            command: vec![
                shell.to_string_lossy().into_owned(),
                String::from("-c"),
                String::from(
                    "printf stdout > \"$HOME/.kraai/data/executions/stdout.bin\" && printf stderr > \"$HOME/.kraai/data/executions/stderr.bin\"",
                ),
            ],
            workspace: workspace.clone(),
            timeout: Duration::from_secs(5),
            network: NetworkPolicy::Disabled,
            environment: BTreeMap::new(),
            extra_programs: Vec::new(),
            cargo_home: None,
            metrics_output: None,
            script_executions_dir: Some(executions.clone()),
            resource_limits: None,
        })?;
        ensure!(
            outcome.success(),
            "sandbox command failed: exit_code={:?}, timed_out={}, stdout={}, stderr={}",
            outcome.exit_code,
            outcome.timed_out,
            String::from_utf8_lossy(&outcome.stdout),
            String::from_utf8_lossy(&outcome.stderr)
        );
        ensure!(
            fs::read(executions.join("stdout.bin"))? == b"stdout",
            "sandbox stdout artifact was not persisted"
        );
        ensure!(
            fs::read(executions.join("stderr.bin"))? == b"stderr",
            "sandbox stderr artifact was not persisted"
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
