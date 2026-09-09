use std::path::Path;
use std::time::Duration;

use kraai_types::SandboxCapability;
use tokio_util::sync::CancellationToken;

use super::{capabilities, find_bwrap, shell_plan, temp_dir};
use crate::{SandboxError, Termination, run};

#[tokio::test]
async fn network_sandbox_exposes_read_only_resolver_configuration() {
    let resolver = Path::new("/etc/resolv.conf");
    if !resolver.exists() {
        return;
    }
    let workspace = temp_dir("dns");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    if find_bwrap(&workspace, &workspace).is_none() {
        std::fs::remove_dir_all(workspace).expect("remove workspace");
        return;
    }
    let mut plan = shell_plan(
        &workspace,
        "while IFS= read -r line || [ -n \"$line\" ]; do printf '%s\\n' \"$line\"; done < /etc/resolv.conf; if ( : >> /etc/resolv.conf ) 2>/dev/null; then exit 1; fi",
        capabilities([SandboxCapability::WorkspaceRead, SandboxCapability::Network]),
        Duration::from_secs(5),
    );
    plan.executable = plan.executable.canonicalize().expect("resolve shell");
    plan.runtime_roots = ["/nix/store", "/usr", "/bin", "/lib", "/lib64"]
        .into_iter()
        .map(Path::new)
        .filter(|path| path.exists())
        .map(Path::to_path_buf)
        .collect();
    let expected = std::fs::read_to_string(resolver).expect("read host resolver");
    let result = run(plan, CancellationToken::new()).await;
    std::fs::remove_dir_all(workspace).expect("remove workspace");
    let output = match result {
        Err(SandboxError::SandboxUnavailable(_)) => return,
        result => result.expect("run sandbox"),
    };
    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        expected.trim_end()
    );
}
