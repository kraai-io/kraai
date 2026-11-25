use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use kraai_execution_types::SandboxCapability;
use kraai_test_support::HANG_TIMEOUT;
use tokio_util::sync::CancellationToken;

use super::{capabilities, shell_plan, temp_dir};
use crate::{SandboxError, run};

const ORIGINAL: &str = "[[profiles]]\nid = \"coding\"\n";

struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = temp_dir(name);
        std::fs::create_dir_all(&path).expect("create workspace");
        Self(path.canonicalize().expect("resolve workspace"))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn plan(&self, script: &str) -> crate::LaunchPlan {
        let mut plan = shell_plan(
            &self.0,
            script,
            capabilities([
                SandboxCapability::HostRead,
                SandboxCapability::WorkspaceWrite,
            ]),
            HANG_TIMEOUT,
        );
        plan.executable = plan.executable.canonicalize().expect("resolve shell");
        plan
    }

    async fn sandbox_available(&self) -> bool {
        let probe = shell_plan(
            &self.0,
            "true",
            capabilities([
                SandboxCapability::HostRead,
                SandboxCapability::WorkspaceRead,
            ]),
            HANG_TIMEOUT,
        );
        !matches!(
            run(probe, CancellationToken::new()).await,
            Err(SandboxError::SandboxUnavailable(_))
        )
    }

    async fn run(&self, script: &str) -> Option<String> {
        if !self.sandbox_available().await {
            return None;
        }
        let output = run(self.plan(script), CancellationToken::new())
            .await
            .expect("run sandbox");
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read configuration")
}

const ATTEMPTS: &str = r#"
for attempt in \
    'printf evil > .kraai/agents.toml' \
    'rm -f .kraai/agents.toml' \
    'mv .kraai .kraai-old' \
    'rm -rf .kraai' \
    'printf evil > .kraai/other.toml'; do
    if sh -c "$attempt" 2>/dev/null; then printf 'allowed: %s\n' "$attempt"; fi
done
printf ok > ordinary
"#;

#[tokio::test]
async fn existing_configuration_is_read_only() {
    let workspace = Workspace::new("config-existing");
    std::fs::create_dir(workspace.path().join(".kraai")).expect("create config");
    std::fs::write(workspace.path().join(".kraai/agents.toml"), ORIGINAL).expect("write");

    let Some(stdout) = workspace.run(ATTEMPTS).await else {
        return;
    };
    assert_eq!(stdout, "");
    assert_eq!(read(&workspace.path().join(".kraai/agents.toml")), ORIGINAL);
    assert!(!workspace.path().join(".kraai/other.toml").exists());
    assert_eq!(read(&workspace.path().join("ordinary")), "ok");
}

#[tokio::test]
async fn missing_configuration_is_not_created_and_cannot_be_planted() {
    let workspace = Workspace::new("config-missing");
    let Some(_) = workspace.run("printf ok > ordinary").await else {
        return;
    };
    assert!(!workspace.path().join(".kraai").exists());
    assert_eq!(read(&workspace.path().join("ordinary")), "ok");

    workspace
        .run("mkdir .kraai && printf evil > .kraai/agents.toml")
        .await
        .expect("sandbox ran before");
    assert!(std::fs::symlink_metadata(workspace.path().join(".kraai")).is_err());

    workspace
        .run("ln -s / .kraai")
        .await
        .expect("sandbox ran before");
    assert!(std::fs::symlink_metadata(workspace.path().join(".kraai")).is_err());
}

#[tokio::test]
async fn symlinked_profiles_and_their_parents_stay_in_place() {
    let workspace = Workspace::new("config-file-link");
    std::fs::create_dir_all(workspace.path().join("shared/profiles")).expect("create target");
    std::fs::write(
        workspace.path().join("shared/profiles/agents.toml"),
        ORIGINAL,
    )
    .expect("write");
    std::fs::create_dir(workspace.path().join(".kraai")).expect("create config");
    symlink(
        "../shared/profiles/agents.toml",
        workspace.path().join(".kraai/agents.toml"),
    )
    .expect("link profiles");

    let script = r#"
for attempt in \
    'printf evil > shared/profiles/agents.toml' \
    'mv shared/profiles shared/old' \
    'mv shared shared-old' \
    'rm -f shared/profiles/agents.toml'; do
    if sh -c "$attempt" 2>/dev/null; then printf 'allowed: %s\n' "$attempt"; fi
done
printf ok > shared/profiles/neighbor
"#;
    let Some(stdout) = workspace.run(script).await else {
        return;
    };
    assert_eq!(stdout, "");
    assert_eq!(read(&workspace.path().join(".kraai/agents.toml")), ORIGINAL);
    assert_eq!(
        read(&workspace.path().join("shared/profiles/neighbor")),
        "ok"
    );
}

#[tokio::test]
async fn replaced_configuration_symlink_is_restored() {
    let workspace = Workspace::new("config-dir-link");
    std::fs::create_dir(workspace.path().join("settings")).expect("create target");
    std::fs::write(workspace.path().join("settings/agents.toml"), ORIGINAL).expect("write");
    symlink("settings", workspace.path().join(".kraai")).expect("link config");

    let Some(_) = workspace
        .run(
            "printf evil > .kraai/agents.toml; rm .kraai && mkdir .kraai && printf evil > .kraai/agents.toml",
        )
        .await
    else {
        return;
    };
    assert_eq!(
        std::fs::read_link(workspace.path().join(".kraai")).expect("config link"),
        PathBuf::from("settings")
    );
    assert_eq!(read(&workspace.path().join(".kraai/agents.toml")), ORIGINAL);
}

#[tokio::test]
async fn workspace_instructions_are_read_only() {
    let workspace = Workspace::new("instructions-existing");
    std::fs::write(workspace.path().join("AGENTS.md"), ORIGINAL).expect("write");

    let script = r#"
for attempt in \
    'printf evil > AGENTS.md' \
    'rm -f AGENTS.md' \
    'mv AGENTS.md moved.md' \
    'ln AGENTS.md alias.md' \
    'ln -sf /etc/passwd AGENTS.md'; do
    if sh -c "$attempt" 2>/dev/null; then printf 'allowed: %s\n' "$attempt"; fi
done
"#;
    let Some(stdout) = workspace.run(script).await else {
        return;
    };
    assert_eq!(stdout, "");
    assert_eq!(read(&workspace.path().join("AGENTS.md")), ORIGINAL);
}

#[tokio::test]
async fn symlinked_instructions_target_is_read_only() {
    let workspace = Workspace::new("instructions-link");
    std::fs::create_dir(workspace.path().join("docs")).expect("create docs");
    std::fs::write(workspace.path().join("docs/agents.md"), ORIGINAL).expect("write");
    symlink("docs/agents.md", workspace.path().join("AGENTS.md")).expect("link");

    let Some(stdout) = workspace
        .run("if printf evil > docs/agents.md 2>/dev/null; then echo allowed; fi; printf ok > docs/other.md")
        .await
    else {
        return;
    };
    assert_eq!(stdout, "");
    assert_eq!(read(&workspace.path().join("AGENTS.md")), ORIGINAL);
    assert_eq!(read(&workspace.path().join("docs/other.md")), "ok");
}

#[tokio::test]
async fn missing_instructions_cannot_be_planted() {
    let workspace = Workspace::new("instructions-missing");
    let Some(_) = workspace.run("printf evil > AGENTS.md").await else {
        return;
    };
    assert!(std::fs::symlink_metadata(workspace.path().join("AGENTS.md")).is_err());

    workspace
        .run("ln -s /etc/passwd AGENTS.md")
        .await
        .expect("sandbox ran before");
    assert!(std::fs::symlink_metadata(workspace.path().join("AGENTS.md")).is_err());
}

#[tokio::test]
async fn unresolvable_or_aliased_configuration_is_rejected() {
    let workspace = Workspace::new("config-unguardable");
    if !workspace.sandbox_available().await {
        return;
    }
    symlink("docs/planted.md", workspace.path().join("AGENTS.md")).expect("link");
    let error = run(workspace.plan("true"), CancellationToken::new())
        .await
        .expect_err("a dangling instructions link cannot be guarded");
    assert!(error.to_string().contains("does not exist"), "{error}");

    std::fs::remove_file(workspace.path().join("AGENTS.md")).expect("remove link");
    std::fs::write(workspace.path().join("AGENTS.md"), ORIGINAL).expect("write");
    std::fs::hard_link(
        workspace.path().join("AGENTS.md"),
        workspace.path().join("editable.md"),
    )
    .expect("hard link");
    let error = run(workspace.plan("true"), CancellationToken::new())
        .await
        .expect_err("a hard-linked instructions file cannot be guarded");
    assert!(error.to_string().contains("hard links"), "{error}");
}

#[test]
fn overlapping_runs_restore_the_first_snapshot() {
    let workspace = Workspace::new("config-overlap");
    let instructions = workspace.path().join("AGENTS.md");
    let first = crate::config_guard::ConfigGuard::capture(workspace.path()).expect("capture");
    std::fs::write(&instructions, "evil").expect("plant instructions");
    let second = crate::config_guard::ConfigGuard::capture(workspace.path()).expect("capture");
    first.restore().expect("restore first");
    std::fs::write(&instructions, "evil").expect("plant again");
    second.restore().expect("restore second");
    assert!(std::fs::symlink_metadata(&instructions).is_err());

    std::fs::write(&instructions, ORIGINAL).expect("write");
    let third = crate::config_guard::ConfigGuard::capture(workspace.path()).expect("capture");
    third.restore().expect("restore third");
    assert_eq!(read(&instructions), ORIGINAL);
}

#[tokio::test]
async fn unreadable_configuration_directory_still_runs_and_stays_closed() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = Workspace::new("config-unreadable");
    let config = workspace.path().join(".kraai");
    std::fs::create_dir(&config).expect("create config");
    std::fs::write(config.join("agents.toml"), ORIGINAL).expect("write");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o000)).expect("lock");

    let script = r#"
for attempt in \
    'chmod 755 .kraai' \
    'mv .kraai .kraai-old' \
    'rm -rf .kraai'; do
    if sh -c "$attempt" 2>/dev/null; then printf 'allowed: %s\n' "$attempt"; fi
done
"#;
    let stdout = workspace.run(script).await;
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o755)).expect("unlock");
    let Some(stdout) = stdout else {
        return;
    };
    assert_eq!(stdout, "");
    assert_eq!(read(&config.join("agents.toml")), ORIGINAL);
}

#[tokio::test]
async fn locked_planted_configuration_is_removed_with_everything_else() {
    let workspace = Workspace::new("config-locked-plant");
    let Some(_) = workspace
        .run("mkdir -p .kraai/inner && printf evil > .kraai/agents.toml && chmod 000 .kraai/inner .kraai && printf evil > AGENTS.md")
        .await
    else {
        return;
    };
    assert!(std::fs::symlink_metadata(workspace.path().join(".kraai")).is_err());
    assert!(std::fs::symlink_metadata(workspace.path().join("AGENTS.md")).is_err());
}

#[tokio::test]
async fn links_through_other_links_are_rejected() {
    let workspace = Workspace::new("config-link-chain");
    if !workspace.sandbox_available().await {
        return;
    }
    std::fs::create_dir(workspace.path().join("real_docs")).expect("create docs");
    std::fs::write(workspace.path().join("real_docs/agents.md"), ORIGINAL).expect("write");
    symlink("real_docs", workspace.path().join("docs")).expect("link docs");
    symlink("docs/agents.md", workspace.path().join("AGENTS.md")).expect("link");
    let error = run(workspace.plan("true"), CancellationToken::new())
        .await
        .expect_err("a link through another link cannot be guarded");
    assert!(
        error.to_string().contains("through another symlink"),
        "{error}"
    );

    std::fs::remove_file(workspace.path().join("AGENTS.md")).expect("remove link");
    symlink("real_docs/agents.md", workspace.path().join("AGENTS.md")).expect("link");
    assert_eq!(workspace.run("true").await.as_deref(), Some(""));
}

#[test]
fn abandoned_runs_still_restore() {
    let workspace = Workspace::new("config-abandoned");
    let instructions = workspace.path().join("AGENTS.md");
    let guard = crate::config_guard::ConfigGuard::capture(workspace.path()).expect("capture");
    std::fs::write(&instructions, "evil").expect("plant instructions");
    drop(guard);
    assert!(std::fs::symlink_metadata(&instructions).is_err());
    let aside = std::fs::read_dir(workspace.path())
        .expect("list workspace")
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("AGENTS.md.sandbox-undone-")
        })
        .expect("the undone file is kept beside the original path");
    assert_eq!(read(&aside.path()), "evil");
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn undone_changes_are_reported() {
    let workspace = Workspace::new("instructions-reported");
    if !workspace.sandbox_available().await {
        return;
    }
    let ordinary = run(
        workspace.plan("printf ok > ordinary"),
        CancellationToken::new(),
    )
    .await
    .expect("run sandbox");
    assert!(!ordinary.protected_entries_undone);
    let planted = run(
        workspace.plan("printf evil > AGENTS.md"),
        CancellationToken::new(),
    )
    .await
    .expect("run sandbox");
    assert!(planted.protected_entries_undone);
}
