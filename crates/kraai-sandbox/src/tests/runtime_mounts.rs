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

#[tokio::test]
async fn path_finds_programs_through_unmounted_profile_symlinks() {
    let base = temp_dir("profile-path");
    let workspace = base.join("workspace");
    let runtime = base.join("store");
    let bin = runtime.join("profile/bin");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&bin).expect("create runtime bin");
    let profile = base.join("profile");
    std::os::unix::fs::symlink(runtime.join("profile"), &profile).expect("link profile");
    let shell = super::executable("sh")
        .canonicalize()
        .expect("resolve shell");
    std::os::unix::fs::symlink(&shell, bin.join("profile-command")).expect("link command");
    let mut plan = shell_plan(
        &workspace,
        "profile-command -c 'printf found'",
        capabilities([SandboxCapability::WorkspaceRead, SandboxCapability::Network]),
        Duration::from_secs(5),
    );
    plan.executable = shell;
    plan.runtime_roots = ["/nix/store", "/usr", "/bin", "/lib", "/lib64"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    plan.runtime_roots.push(runtime);
    plan.environment
        .insert("PATH".into(), profile.join("bin").into_os_string());
    let result = run(plan, CancellationToken::new()).await;
    std::fs::remove_dir_all(base).expect("remove fixture");
    let output = match result {
        Err(SandboxError::SandboxUnavailable(_)) => return,
        result => result.expect("run sandbox"),
    };
    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"found");
}

#[test]
fn path_resolution_preserves_search_order_and_unresolved_entries() {
    let base = temp_dir("path-resolution");
    let bin = base.join("store/bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    let profile = base.join("profile");
    std::os::unix::fs::symlink(&bin, &profile).expect("link profile");
    let entries = vec![
        profile,
        std::path::PathBuf::new(),
        std::path::PathBuf::from("relative/bin"),
        base.join("missing"),
        bin.clone(),
    ];
    let path = std::env::join_paths(&entries).expect("join path");
    let resolved = crate::platform::linux::resolve_search_path(&path);
    let mut expected = entries;
    expected.insert(1, bin.canonicalize().expect("resolve bin"));
    assert_eq!(
        std::env::split_paths(&resolved).collect::<Vec<_>>(),
        expected
    );
    std::fs::remove_dir_all(base).expect("remove fixture");
}
