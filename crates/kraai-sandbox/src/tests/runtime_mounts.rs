use std::time::Duration;

use kraai_types::SandboxCapability;
use tokio_util::sync::CancellationToken;

use super::{capabilities, find_bwrap, shell_plan, temp_dir};
use crate::{SandboxError, Termination, run};

#[tokio::test]
async fn executable_in_aliased_workspace_runs_without_host_read() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = crate::temp_dir::PrivateTempDir::create(None).expect("create fixture");
    let workspace = fixture.path().join("workspace");
    let alias = fixture.path().join("alias");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::os::unix::fs::symlink(&workspace, &alias).expect("link workspace");
    let tool = workspace.join("tool");
    std::fs::write(
        &tool,
        "#!/bin/sh\nprintf allowed > ordinary\nprintf success\n",
    )
    .expect("write executable");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700))
        .expect("make executable");
    let mut plan = shell_plan(
        &alias,
        "",
        capabilities([
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::Network,
        ]),
        Duration::from_secs(5),
    );
    plan.executable = alias.join("tool");
    plan.args.clear();
    plan.runtime_roots = ["/nix/store", "/usr", "/bin", "/lib", "/lib64"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    plan.runtime_roots.push(alias);
    let output = match run(plan, CancellationToken::new()).await {
        Err(SandboxError::SandboxUnavailable(_)) => return,
        result => result.expect("run sandbox"),
    };
    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"success");
    assert!(workspace.join("ordinary").exists());
}

#[tokio::test]
async fn external_symlink_executable_runs_at_its_declared_runtime_root() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = crate::temp_dir::PrivateTempDir::create(None).expect("create fixture");
    let workspace = fixture.path().join("workspace");
    let external = fixture.path().join("external");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::fs::create_dir(&external).expect("create external directory");
    let target = external.join("tool");
    std::fs::write(&target, "#!/bin/sh\nprintf success\n").expect("write executable");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700))
        .expect("make executable");
    let executable = fixture.path().join("tool-alias");
    std::os::unix::fs::symlink(&target, &executable).expect("link executable");
    let mut plan = shell_plan(
        &workspace,
        "",
        capabilities([SandboxCapability::WorkspaceRead, SandboxCapability::Network]),
        Duration::from_secs(5),
    );
    plan.executable = executable.clone();
    plan.args.clear();
    plan.runtime_roots = ["/nix/store", "/usr", "/bin", "/lib", "/lib64"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    plan.runtime_roots.push(executable);
    let output = match run(plan, CancellationToken::new()).await {
        Err(SandboxError::SandboxUnavailable(_)) => return,
        result => result.expect("run sandbox"),
    };
    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"success");
}

#[tokio::test]
async fn skill_runtime_root_is_read_only_with_default_capabilities() {
    let base = temp_dir("skill-read-root");
    let workspace = base.join("workspace");
    let skill = base.join("store/unslop");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&skill).expect("create skill");
    std::fs::write(skill.join("SKILL.md"), "instructions").expect("write skill");
    std::fs::write(skill.join("reference.txt"), "reference").expect("write reference");
    std::fs::write(base.join("secret"), "secret\n").expect("write unrelated file");
    let mut plan = shell_plan(
        &workspace,
        "read -r text < \"$SKILL/SKILL.md\"; test \"$text\" = instructions || exit 1; read -r text < \"$SKILL/reference.txt\"; test \"$text\" = reference || exit 2; if (printf bad > \"$SKILL/SKILL.md\"); then exit 3; fi; if (read -r text < \"$SECRET\"); then exit 4; fi",
        capabilities([SandboxCapability::WorkspaceRead]),
        Duration::from_secs(5),
    );
    plan.executable = plan.executable.canonicalize().expect("resolve shell");
    plan.runtime_roots = ["/nix/store", "/usr", "/bin", "/lib", "/lib64"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    plan.runtime_roots.push(skill.clone());
    plan.environment
        .insert("SKILL".into(), skill.into_os_string());
    plan.environment
        .insert("SECRET".into(), base.join("secret").into_os_string());
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
}

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
