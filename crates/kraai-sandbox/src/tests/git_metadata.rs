use std::time::Duration;

use kraai_types::SandboxCapability;
use tokio_util::sync::CancellationToken;

use crate::temp_dir::PrivateTempDir;
use crate::tests::{capabilities, shell_plan};
use crate::{Termination, run};

#[tokio::test]
async fn external_git_metadata_does_not_grant_host_read() {
    let fixture = PrivateTempDir::create(None).expect("create fixture");
    let workspace = fixture.path().join("workspace");
    let external = fixture.path().join("host-metadata");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::fs::create_dir(&external).expect("create external metadata");
    std::fs::write(external.join("secret"), "private\n").expect("write host secret");
    std::fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", external.display()),
    )
    .expect("write Git pointer");
    let mut plan = shell_plan(
        &workspace,
        r#"
            printf allowed > ordinary || exit 1
            if (read -r secret < "$EXTERNAL/secret"); then exit 2; fi
            printf restricted
        "#,
        capabilities([
            SandboxCapability::WorkspaceWrite,
            SandboxCapability::Network,
        ]),
        Duration::from_secs(5),
    );
    plan.environment
        .insert("EXTERNAL".into(), external.into_os_string());
    plan.runtime_roots = ["/nix/store", "/usr", "/bin", "/lib", "/lib64"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .collect();

    let output = match run(plan, CancellationToken::new()).await {
        #[cfg(target_os = "linux")]
        Err(crate::SandboxError::SandboxUnavailable(_)) => return,
        result => result.expect("run sandbox"),
    };

    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"restricted");
}

#[test]
#[cfg(target_os = "linux")]
fn unprotectable_linux_metadata_symlinks_fail_closed() {
    let fixture = PrivateTempDir::create(None).expect("create fixture");
    let target = fixture.path().join("target");
    std::fs::create_dir(&target).expect("create metadata target");
    std::os::unix::fs::symlink(&target, fixture.path().join(".agents")).expect("link metadata");
    let plan = shell_plan(
        fixture.path(),
        "true",
        capabilities([SandboxCapability::WorkspaceWrite]),
        Duration::from_secs(5),
    );
    let error = crate::platform::linux::build_bwrap_args(&plan, fixture.path())
        .expect_err("Linux metadata symlink must fail closed");
    assert!(error.to_string().contains("symlinked workspace metadata"));
}

#[tokio::test]
async fn git_pointer_targets_require_metadata_write() {
    for (metadata_write, linked_pointer, alias_workspace) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (false, false, true),
        (true, false, true),
        (false, true, true),
        (true, true, true),
    ] {
        let fixture = PrivateTempDir::create(None).expect("create fixture");
        let workspace = fixture.path().join("workspace");
        let git_dir = workspace.join("metadata/worktree");
        let common_dir = workspace.join("metadata/common");
        std::fs::create_dir_all(&git_dir).expect("create Git directory");
        std::fs::create_dir_all(&common_dir).expect("create common directory");
        std::fs::write(workspace.join(".git"), "gitdir: metadata/worktree\n")
            .expect("write Git pointer");
        let pointer = if linked_pointer {
            let pointer = workspace.join("common-pointer");
            std::os::unix::fs::symlink(&pointer, git_dir.join("commondir"))
                .expect("link common pointer");
            pointer
        } else {
            git_dir.join("commondir")
        };
        std::fs::write(&pointer, "../common\n").expect("write common pointer");
        std::fs::write(git_dir.join("HEAD"), "original").expect("write worktree metadata");
        std::fs::write(common_dir.join("config"), "original").expect("write common metadata");
        let launch_workspace = if alias_workspace {
            let alias = fixture.path().join("alias");
            std::os::unix::fs::symlink(&workspace, &alias).expect("link workspace");
            alias
        } else {
            workspace.clone()
        };
        let mut plan = shell_plan(
            &launch_workspace,
            r#"
                printf allowed > ordinary || exit 1
                for file in .git metadata/worktree/HEAD metadata/common/config "$COMMON_POINTER"; do
                    if (printf changed > "$file"); then printf 'writable\n'; else printf 'readonly\n'; fi
                done
            "#,
            capabilities([
                SandboxCapability::HostRead,
                SandboxCapability::Network,
                if metadata_write {
                    SandboxCapability::MetadataWrite
                } else {
                    SandboxCapability::WorkspaceWrite
                },
            ]),
            Duration::from_secs(5),
        );

        plan.environment
            .insert("COMMON_POINTER".into(), pointer.into_os_string());
        let output = match run(plan, CancellationToken::new()).await {
            #[cfg(target_os = "linux")]
            Err(crate::SandboxError::SandboxUnavailable(_)) => return,
            result => result.expect("run sandbox"),
        };

        assert_eq!(
            output.termination,
            Termination::Exited { code: Some(0) },
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status = if metadata_write {
            "writable\n"
        } else {
            "readonly\n"
        };
        assert_eq!(
            output.stdout,
            status.repeat(4).as_bytes(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: &[u8] = if metadata_write {
            b"changed"
        } else {
            b"original"
        };
        for file in [git_dir.join("HEAD"), common_dir.join("config")] {
            assert_eq!(std::fs::read(file).expect("read metadata"), expected);
        }
        assert!(workspace.join("ordinary").exists());
    }
}
