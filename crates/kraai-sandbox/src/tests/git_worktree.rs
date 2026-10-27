use std::path::PathBuf;

use crate::platform::git_worktree::linked_worktree_common_dir;
use crate::temp_dir::PrivateTempDir;

pub(crate) struct LinkedWorktree {
    _root: PrivateTempDir,
    pub(crate) workspace: PathBuf,
    pub(crate) git_dir: PathBuf,
    pub(crate) common_dir: PathBuf,
}

impl LinkedWorktree {
    pub(crate) fn new() -> Self {
        let root = PrivateTempDir::create(None).expect("create fixture");
        let base = root.path().canonicalize().expect("resolve fixture");
        let workspace = base.join("worktree");
        let common_dir = base.join("repo/.git");
        let git_dir = common_dir.join("worktrees/worktree");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::fs::create_dir_all(&git_dir).expect("create worktree git dir");
        std::fs::write(
            workspace.join(".git"),
            "gitdir: ../repo/.git/worktrees/worktree\n",
        )
        .expect("write git pointer");
        std::fs::write(git_dir.join("commondir"), "../..\n").expect("write common pointer");
        std::fs::write(
            git_dir.join("gitdir"),
            format!("{}\n", workspace.join(".git").display()),
        )
        .expect("write back pointer");
        Self {
            _root: root,
            workspace,
            git_dir,
            common_dir,
        }
    }
}

#[test]
fn normal_checkout_has_no_linked_git_dir() {
    let root = PrivateTempDir::create(None).expect("create fixture");
    std::fs::create_dir(root.path().join(".git")).expect("create git dir");
    assert_eq!(linked_worktree_common_dir(root.path()), None);
}

#[test]
fn linked_worktree_resolves_relative_common_dir() {
    let worktree = LinkedWorktree::new();
    assert_eq!(
        linked_worktree_common_dir(&worktree.workspace),
        Some(worktree.common_dir)
    );
}

type Corruption = fn(&LinkedWorktree);

#[test]
fn malformed_linked_worktrees_are_ignored() {
    let cases: [(&str, Corruption); 6] = [
        ("missing prefix", |worktree| {
            std::fs::write(
                worktree.workspace.join(".git"),
                "../repo/.git/worktrees/worktree",
            )
            .expect("rewrite git pointer");
        }),
        ("empty git pointer", |worktree| {
            std::fs::write(worktree.workspace.join(".git"), "gitdir: \n")
                .expect("rewrite git pointer");
        }),
        ("missing common dir", |worktree| {
            std::fs::remove_file(worktree.git_dir.join("commondir")).expect("remove commondir");
        }),
        ("foreign back pointer", |worktree| {
            std::fs::write(worktree.git_dir.join("gitdir"), "/elsewhere/.git\n")
                .expect("rewrite back pointer");
        }),
        ("common dir is not the worktree parent", |worktree| {
            std::fs::write(worktree.git_dir.join("commondir"), "..\n").expect("rewrite commondir");
        }),
        ("git pointer to an arbitrary directory", |worktree| {
            std::fs::write(
                worktree.workspace.join(".git"),
                format!("gitdir: {}\n", worktree.common_dir.display()),
            )
            .expect("rewrite git pointer");
            std::fs::write(worktree.common_dir.join("commondir"), ".\n").expect("write commondir");
            std::fs::write(
                worktree.common_dir.join("gitdir"),
                format!("{}\n", worktree.workspace.join(".git").display()),
            )
            .expect("write back pointer");
        }),
    ];
    for (name, corrupt) in cases {
        let worktree = LinkedWorktree::new();
        corrupt(&worktree);
        assert_eq!(
            linked_worktree_common_dir(&worktree.workspace),
            None,
            "{name}"
        );
    }
}

#[test]
fn common_dir_inside_workspace_is_ignored() {
    let root = PrivateTempDir::create(None).expect("create fixture");
    let workspace = root.path().canonicalize().expect("resolve fixture");
    let git_dir = workspace.join("common/worktrees/name");
    std::fs::create_dir_all(&git_dir).expect("create worktree git dir");
    std::fs::write(workspace.join(".git"), "gitdir: common/worktrees/name\n")
        .expect("write git pointer");
    std::fs::write(git_dir.join("commondir"), "../..\n").expect("write common pointer");
    std::fs::write(
        git_dir.join("gitdir"),
        format!("{}\n", workspace.join(".git").display()),
    )
    .expect("write back pointer");
    assert_eq!(linked_worktree_common_dir(&workspace), None);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::time::Duration;

    use kraai_execution_types::SandboxCapability;
    use kraai_test_support::HANG_TIMEOUT;
    use tokio_util::sync::CancellationToken;

    use super::LinkedWorktree;
    use crate::tests::{build_bwrap_args, capabilities, contains_mount, shell_plan};
    use crate::{SandboxError, Termination, run};

    #[test]
    fn bwrap_binds_linked_common_dir_only_with_workspace_write() {
        let worktree = LinkedWorktree::new();
        let private_temp = crate::temp_dir::PrivateTempDir::create(None).expect("private temp");
        for (granted, writable) in [
            (SandboxCapability::WorkspaceWrite, true),
            (SandboxCapability::WorkspaceRead, false),
        ] {
            let plan = shell_plan(
                &worktree.workspace,
                "true",
                capabilities([granted]),
                Duration::from_secs(1),
            );
            let args = build_bwrap_args(&plan, private_temp.path()).expect("build args");
            assert_eq!(
                contains_mount(&args, "--bind", &worktree.common_dir),
                writable
            );
            assert!(!args.iter().any(|arg| arg == worktree.git_dir.as_os_str()));
        }
    }

    #[tokio::test]
    async fn linked_worktree_git_dir_is_writable_with_workspace_write() {
        check_git_dir_write(true).await;
    }

    #[tokio::test]
    async fn linked_worktree_git_dir_stays_read_only_without_workspace_write() {
        check_git_dir_write(false).await;
    }

    async fn check_git_dir_write(writable: bool) {
        let worktree = LinkedWorktree::new();
        let granted = if writable {
            vec![SandboxCapability::WorkspaceWrite]
        } else {
            vec![
                SandboxCapability::WorkspaceRead,
                SandboxCapability::HostRead,
            ]
        };
        let mut plan = shell_plan(
            &worktree.workspace,
            "if (printf lock > \"$GIT_DIR/index.lock\"); then printf writable; else printf readonly; fi",
            capabilities(granted),
            HANG_TIMEOUT,
        );
        plan.executable = plan.executable.canonicalize().expect("resolve shell");
        plan.runtime_roots = ["/nix/store", "/usr", "/bin", "/lib", "/lib64"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .filter(|path| path.exists())
            .collect();
        plan.environment
            .insert("GIT_DIR".into(), worktree.git_dir.clone().into_os_string());
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
        assert_eq!(
            output.stdout,
            if writable { "writable" } else { "readonly" }.as_bytes()
        );
        assert_eq!(worktree.git_dir.join("index.lock").exists(), writable);
    }
}
