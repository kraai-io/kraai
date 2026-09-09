use std::time::Duration;

use kraai_types::SandboxCapability;
use tokio_util::sync::CancellationToken;

use super::{capabilities, find_bwrap, shell_plan, temp_dir};
use crate::{SandboxError, Termination, run};

#[tokio::test]
async fn workspace_write_survives_overlapping_runtime_roots() {
    check_runtime_mounts(true).await;
}

#[tokio::test]
async fn overlapping_runtime_roots_do_not_grant_workspace_write() {
    check_runtime_mounts(false).await;
}

async fn check_runtime_mounts(writable: bool) {
    for overlap in ["nested", "equal", "ancestor"] {
        let base = temp_dir(overlap);
        let workspace = base.join("workspace");
        let build = workspace.join("target/debug");
        std::fs::create_dir_all(&build).expect("create build directory");
        std::fs::create_dir_all(workspace.join(".git")).expect("create metadata");
        if find_bwrap(&workspace, &base).is_none() {
            std::fs::remove_dir_all(base).expect("remove fixture");
            return;
        }
        let mut granted = vec![SandboxCapability::HostRead, SandboxCapability::Network];
        if writable {
            granted.push(SandboxCapability::WorkspaceWrite);
        }
        let mut plan = shell_plan(
            &workspace,
            "if (printf lock > target/debug/.cargo-build-lock); then printf writable; else printf readonly; fi; if (printf bad > .git/forbidden); then exit 1; fi; if (printf bad > ../forbidden); then exit 2; fi",
            capabilities(granted),
            Duration::from_secs(5),
        );
        plan.runtime_roots.push(match overlap {
            "nested" => build.clone(),
            "equal" => workspace.clone(),
            _ => base.clone(),
        });
        let result = run(plan, CancellationToken::new()).await;
        let wrote_lock = build.join(".cargo-build-lock").exists();
        std::fs::remove_dir_all(base).expect("remove fixture");
        let output = match result {
            Err(SandboxError::SandboxUnavailable(_)) => return,
            result => result.expect("run sandbox"),
        };
        assert_eq!(
            output.termination,
            Termination::Exited { code: Some(0) },
            "{overlap}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            wrote_lock,
            writable,
            "{overlap}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
