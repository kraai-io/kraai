use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use kraai_types::SandboxCapability;
use tokio::process::Command;

use crate::config::LaunchPlan;
use crate::error::SandboxError;

use super::seccomp::restricted_network_seccomp_filter;

const BWRAP_PROGRAM: &str = "bwrap";
const BWRAP_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const BWRAP_SECCOMP_STDIN_FD: &str = "0";
use crate::platform::metadata::protected_paths;

type BwrapProbeCache = BTreeMap<(PathBuf, bool), Result<(), String>>;

static BWRAP_PROBE_CACHE: OnceLock<Mutex<BwrapProbeCache>> = OnceLock::new();

pub(super) async fn ensure_bwrap_sandbox_available(
    bwrap: &Path,
    network_enabled: bool,
) -> Result<(), SandboxError> {
    let key = (bwrap.to_path_buf(), network_enabled);
    let cache = BWRAP_PROBE_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));

    {
        let guard = cache.lock().map_err(|error| {
            SandboxError::SandboxUnavailable(format!(
                "bubblewrap sandbox capability cache is unavailable: {error}"
            ))
        })?;
        if let Some(result) = guard.get(&key).cloned() {
            return result.map_err(SandboxError::SandboxUnavailable);
        }
    }

    let result = run_bwrap_sandbox_probe(bwrap, network_enabled, BWRAP_PROBE_TIMEOUT).await;
    cache
        .lock()
        .map_err(|error| {
            SandboxError::SandboxUnavailable(format!(
                "bubblewrap sandbox capability cache is unavailable: {error}"
            ))
        })?
        .insert(key, result.clone());
    result.map_err(SandboxError::SandboxUnavailable)
}

pub(crate) async fn run_bwrap_sandbox_probe(
    bwrap: &Path,
    network_enabled: bool,
    timeout: Duration,
) -> Result<(), String> {
    let mut process = Command::new(bwrap);
    let seccomp_filter = restricted_network_seccomp_filter(network_enabled, &[])
        .map_err(|error| format!("unable to prepare bubblewrap seccomp probe: {error}"))?;
    let stdin = seccomp_filter.map_or_else(Stdio::null, Stdio::from);
    process
        .args(build_bwrap_probe_args(network_enabled))
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = process
        .spawn()
        .map_err(|error| format!("unable to run bubblewrap sandbox probe: {error}"))?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(format!(
                "unable to wait for bubblewrap sandbox probe: {error}"
            ));
        }
        Err(_) => {
            return Err(format!(
                "bubblewrap sandbox probe timed out after {} second(s)",
                timeout.as_secs()
            ));
        }
    };

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(bwrap_probe_failure_message(network_enabled, &stderr))
}

pub(crate) fn build_bwrap_probe_args(network_enabled: bool) -> Vec<OsString> {
    let mut args = vec![
        "--new-session".into(),
        "--die-with-parent".into(),
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--dev".into(),
        "/dev".into(),
        "--unshare-all".into(),
    ];
    if network_enabled {
        args.push("--share-net".into());
    } else {
        args.push("--seccomp".into());
        args.push(BWRAP_SECCOMP_STDIN_FD.into());
    }
    args.extend(["--chdir".into(), "/".into(), "--".into(), "true".into()]);
    args
}

pub(crate) fn bwrap_probe_failure_message(network_enabled: bool, stderr: &str) -> String {
    if !network_enabled && is_network_namespace_error(stderr) {
        return String::from(
            "bubblewrap cannot create a network namespace on this host; request no-sandbox or the network capability",
        );
    }
    let detail = stderr.trim();
    if detail.is_empty() {
        return String::from("bubblewrap sandbox probe failed");
    }
    format!("bubblewrap sandbox probe failed: {detail}")
}

fn is_network_namespace_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("netlink_route")
        || lower.contains("network namespace")
        || lower.contains("newnet")
        || lower.contains("unshare-net")
}

pub(crate) fn build_bwrap_args(
    plan: &LaunchPlan,
    private_temp: &Path,
) -> Result<Vec<OsString>, SandboxError> {
    let capabilities = &plan.capabilities;
    let network_enabled = capabilities.contains(SandboxCapability::Network);
    let mut args = vec![
        "--new-session".into(),
        "--die-with-parent".into(),
        "--unshare-all".into(),
    ];
    if network_enabled {
        args.push("--share-net".into());
    }

    if capabilities.contains(SandboxCapability::HostWrite) {
        push_bind(&mut args, "--bind", Path::new("/"));
    } else if capabilities.contains(SandboxCapability::HostRead) {
        push_bind(&mut args, "--ro-bind", Path::new("/"));
    } else {
        args.extend([
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
        ]);
    }

    let resolved_workspace = plan.workspace_root.canonicalize().map_err(|error| {
        SandboxError::SandboxUnavailable(format!("unable to resolve workspace: {error}"))
    })?;
    for root in deduplicated_roots(&plan.runtime_roots) {
        // The workspace mount already exposes these paths. A separate recursive
        // read-only bind would override workspace-write even after rebinding its parent.
        // Keep mounts for symlinks that resolve outside the workspace.
        if root
            .canonicalize()
            .is_ok_and(|resolved| resolved.starts_with(&resolved_workspace))
        {
            continue;
        }
        push_parent_dirs(&mut args, &root);
        push_bind(&mut args, "--ro-bind", &root);
    }

    // Workspace permissions take precedence over overlapping runtime roots, such as
    // target/debug when running a development binary from the active workspace.
    let workspace_flag = if capabilities.contains(SandboxCapability::WorkspaceWrite) {
        "--bind"
    } else {
        "--ro-bind"
    };
    push_parent_dirs(&mut args, &resolved_workspace);
    push_bind(&mut args, workspace_flag, &resolved_workspace);

    if network_enabled
        && !capabilities.contains(SandboxCapability::HostRead)
        && !capabilities.contains(SandboxCapability::HostWrite)
    {
        // Network clients also need DNS configuration and the host's public CA bundles.
        // Bind files individually so source symlinks work without exposing their parent
        // directories or private keys. Missing distribution-specific paths are optional.
        for path in [
            "/etc/resolv.conf",
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/ssl/cert.pem",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/pki/tls/cert.pem",
            "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
        ] {
            push_bind(&mut args, "--ro-bind-try", Path::new(path));
        }
    }

    if capabilities.contains(SandboxCapability::WorkspaceWrite)
        && !capabilities.contains(SandboxCapability::MetadataWrite)
    {
        for protected in protected_paths(&resolved_workspace)?
            .into_iter()
            .filter(|path| path.starts_with(&resolved_workspace) && path.exists())
        {
            if std::fs::symlink_metadata(&protected)
                .map_err(|error| {
                    SandboxError::SandboxUnavailable(format!("unable to inspect metadata: {error}"))
                })?
                .file_type()
                .is_symlink()
            {
                return Err(SandboxError::SandboxUnavailable(String::from(
                    "symlinked workspace metadata requires metadata-write on Linux",
                )));
            }
            push_parent_dirs(&mut args, &protected);
            push_bind(&mut args, "--ro-bind", &protected);
        }
    }

    push_parent_dirs(&mut args, private_temp);
    push_bind(&mut args, "--bind", private_temp);

    if !network_enabled {
        args.push("--seccomp".into());
        args.push(BWRAP_SECCOMP_STDIN_FD.into());
    }

    let executable = plan.executable.canonicalize().map_err(|error| {
        SandboxError::SandboxUnavailable(format!("unable to resolve executable: {error}"))
    })?;
    let executable = if executable.starts_with(&resolved_workspace) {
        executable
    } else {
        plan.executable.clone()
    };
    args.push("--chdir".into());
    args.push(resolved_workspace.into_os_string());
    args.push("--".into());
    args.push(executable.into_os_string());
    args.extend(plan.args.iter().cloned());
    Ok(args)
}

fn deduplicated_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn push_parent_dirs(args: &mut Vec<OsString>, path: &Path) {
    let mut parents = path.ancestors().skip(1).collect::<Vec<_>>();
    parents.reverse();
    for parent in parents
        .into_iter()
        .filter(|parent| *parent != Path::new("/"))
    {
        args.push("--dir".into());
        args.push(parent.as_os_str().to_os_string());
    }
}

fn push_bind(args: &mut Vec<OsString>, flag: &str, path: &Path) {
    args.push(flag.into());
    args.push(path.as_os_str().to_os_string());
    args.push(path.as_os_str().to_os_string());
}

pub(crate) fn find_bwrap(workspace_dir: &Path, private_temp: &Path) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let cwd = std::env::current_dir().ok()?;
    let disallowed_roots = [workspace_dir, private_temp, std::env::temp_dir().as_path()]
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();

    std::env::split_paths(&path_var).find_map(|entry| {
        let base = if entry.is_absolute() {
            entry
        } else {
            cwd.join(entry)
        };
        let candidate = base.join(BWRAP_PROGRAM);
        if !candidate.is_file() {
            return None;
        }
        let canonical = candidate.canonicalize().unwrap_or(candidate);
        if disallowed_roots
            .iter()
            .any(|root| canonical.starts_with(root))
        {
            return None;
        }
        Some(canonical)
    })
}
