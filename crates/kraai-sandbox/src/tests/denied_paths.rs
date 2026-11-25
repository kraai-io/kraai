use std::path::{Path, PathBuf};

use kraai_execution_types::SandboxCapability;
use kraai_test_support::HANG_TIMEOUT;
use tokio_util::sync::CancellationToken;

use super::{capabilities, shell_plan};
use crate::temp_dir::PrivateTempDir;
use crate::{ExecutionOutput, LaunchPlan, SandboxError, Termination, run};

fn system_roots() -> Vec<PathBuf> {
    ["/nix/store", "/usr", "/bin", "/lib", "/lib64"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect()
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(path, contents).expect("write fixture");
}

async fn run_succeeds(plan: LaunchPlan) -> Option<ExecutionOutput> {
    let output = match run(plan, CancellationToken::new()).await {
        Err(SandboxError::SandboxUnavailable(_)) => return None,
        result => result.expect("run sandbox"),
    };
    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(output)
}

#[tokio::test]
async fn storage_inside_writable_workspace_is_hidden_except_granted_output() {
    let fixture = PrivateTempDir::create(None).expect("create fixture");
    let home = fixture.path().join("home");
    let storage = home.join(".kraai");
    write(&storage.join("providers.toml"), "api_key = 'secret'\n");
    write(
        &storage.join("provider-state/openai-codex/auth.json"),
        "refresh-token\n",
    );
    write(&storage.join("script-output/session/stdout"), "output\n");
    let mut plan = shell_plan(
        &home,
        "if (cat .kraai/providers.toml); then exit 1; fi; \
         if (cat .kraai/provider-state/openai-codex/auth.json); then exit 2; fi; \
         if (printf bad > .kraai/providers.toml); then exit 3; fi; \
         if (mkdir .kraai/data); then exit 4; fi; \
         read -r text < .kraai/script-output/session/stdout; test \"$text\" = output || exit 5; \
         if (printf bad > .kraai/script-output/session/stdout); then exit 6; fi; \
         printf ok > ordinary || exit 7",
        capabilities([
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::HostRead,
        ]),
        HANG_TIMEOUT,
    );
    plan.runtime_roots
        .push(storage.join("script-output/session"));
    plan.denied_paths.push(storage.clone());
    if run_succeeds(plan).await.is_none() {
        return;
    }
    assert_eq!(
        std::fs::read_to_string(storage.join("providers.toml")).expect("read providers"),
        "api_key = 'secret'\n"
    );
    assert!(!storage.join("data").exists());
    assert!(home.join("ordinary").exists());
}

#[tokio::test]
async fn host_write_cannot_read_or_change_denied_paths() {
    let fixture = PrivateTempDir::create(None).expect("create fixture");
    let workspace = fixture.path().join("workspace");
    let state = fixture.path().join("state");
    let secret = fixture.path().join("providers.toml");
    write(&workspace.join(".kraai/agents.toml"), "profiles\n");
    write(&state.join("auth.json"), "refresh-token\n");
    write(&secret, "api_key = 'secret'\n");
    let mut plan = shell_plan(
        &workspace,
        "if (cat \"$STATE/auth.json\"); then exit 1; fi; \
         test -z \"$(ls -A \"$STATE\")\" || exit 2; \
         if (cat \"$SECRET\"); then exit 3; fi; \
         if (printf bad > \"$SECRET\"); then exit 4; fi; \
         if (printf bad > \"$STATE/auth.json\"); then exit 5; fi; \
         if (rm \"$SECRET\"); then exit 6; fi; \
         read -r text < .kraai/agents.toml; test \"$text\" = profiles || exit 7",
        capabilities([
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::HostRead,
            SandboxCapability::HostWrite,
        ]),
        HANG_TIMEOUT,
    );
    plan.environment
        .insert("STATE".into(), state.clone().into_os_string());
    plan.environment
        .insert("SECRET".into(), secret.clone().into_os_string());
    plan.denied_paths = vec![
        state.clone(),
        secret.clone(),
        fixture.path().join("missing/providers.toml"),
    ];
    if run_succeeds(plan).await.is_none() {
        return;
    }
    assert_eq!(
        std::fs::read_to_string(state.join("auth.json")).expect("read auth"),
        "refresh-token\n"
    );
    assert_eq!(
        std::fs::read_to_string(&secret).expect("read secret"),
        "api_key = 'secret'\n"
    );
}

#[tokio::test]
async fn denied_path_is_hidden_behind_a_symlinked_runtime_root() {
    let fixture = PrivateTempDir::create(None).expect("create fixture");
    let workspace = fixture.path().join("workspace");
    let home = fixture.path().join("home");
    let alias = fixture.path().join("alias");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    write(&home.join(".kraai/providers.toml"), "api_key = 'secret'\n");
    write(&home.join("notes"), "notes\n");
    std::os::unix::fs::symlink(&home, &alias).expect("link home");
    let mut plan = shell_plan(
        &workspace,
        "read -r text < \"$ALIAS/notes\"; test \"$text\" = notes || exit 1; \
         if (cat \"$ALIAS/.kraai/providers.toml\"); then exit 2; fi",
        capabilities([SandboxCapability::WorkspaceRead]),
        HANG_TIMEOUT,
    );
    plan.executable = plan.executable.canonicalize().expect("resolve shell");
    plan.runtime_roots = system_roots();
    plan.runtime_roots.push(alias.clone());
    plan.environment
        .insert("ALIAS".into(), alias.into_os_string());
    plan.denied_paths.push(home.join(".kraai"));
    run_succeeds(plan).await;
}

#[tokio::test]
async fn workspace_inside_denied_storage_stays_writable() {
    let fixture = PrivateTempDir::create(None).expect("create fixture");
    let storage = fixture.path().join("storage");
    let workspace = storage.join("eval/run");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    write(&storage.join("providers.toml"), "api_key = 'secret'\n");
    let mut plan = shell_plan(
        &workspace,
        "if (cat \"$STORAGE/providers.toml\"); then exit 1; fi; \
         if (printf bad > \"$STORAGE/created\"); then exit 2; fi; \
         printf ok > ordinary || exit 3",
        capabilities([
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::HostRead,
        ]),
        HANG_TIMEOUT,
    );
    plan.environment
        .insert("STORAGE".into(), storage.clone().into_os_string());
    plan.denied_paths.push(storage.clone());
    if run_succeeds(plan).await.is_none() {
        return;
    }
    assert!(workspace.join("ordinary").exists());
    assert!(!storage.join("created").exists());
}
