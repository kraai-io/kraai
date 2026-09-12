use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::symlink;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kraai_types::SandboxCapability;
use tokio_util::sync::CancellationToken;

use super::{capabilities, temp_dir};
use crate::platform::PROTECTED_METADATA_NAMES;
use crate::{ExecutionOutput, LaunchPlan, PrivateTempConfig, Termination, run};

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str) -> Self {
        let path = temp_dir(name);
        std::fs::create_dir_all(path.join("workspace")).expect("create workspace");
        Self(path)
    }

    fn workspace(&self) -> PathBuf {
        self.0.join("workspace")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn shell_plan(
    workspace: &Path,
    script: &str,
    granted: impl IntoIterator<Item = SandboxCapability>,
) -> LaunchPlan {
    let mut plan = LaunchPlan::new(
        PathBuf::from("/bin/sh"),
        workspace.to_path_buf(),
        capabilities(granted),
        Duration::from_secs(10),
    );
    plan.args = vec![OsString::from("-c"), OsString::from(script)];
    plan.runtime_roots = vec![PathBuf::from("/bin")];
    plan.environment
        .insert("PATH".into(), "/bin:/usr/bin".into());
    plan
}

async fn successful_run(plan: LaunchPlan) -> ExecutionOutput {
    let output = run(plan, CancellationToken::new())
        .await
        .expect("macOS sandbox must be available");
    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[tokio::test]
async fn capabilities_enforce_workspace_and_host_boundaries() {
    for (capability, workspace_write, host_read, host_write) in [
        (SandboxCapability::WorkspaceRead, false, false, false),
        (SandboxCapability::WorkspaceWrite, true, false, false),
        (SandboxCapability::HostRead, false, true, false),
        (SandboxCapability::HostWrite, true, true, true),
    ] {
        let fixture = Fixture::new("macos-boundaries");
        std::fs::write(fixture.workspace().join("input"), "workspace\n")
            .expect("write workspace input");
        std::fs::write(fixture.0.join("secret"), "host\n").expect("write host input");
        let mut plan = shell_plan(
            &fixture.workspace(),
            r#"
                read -r value < input && test "$value" = workspace || exit 1
                if (printf changed > output); then printf w; fi
                if (read -r value < "$HOST/secret" && test "$value" = host); then printf r; fi
                if (printf changed > "$HOST/output"); then printf h; fi
                exit 0
            "#,
            [capability],
        );
        plan.environment
            .insert("HOST".into(), fixture.0.as_os_str().to_os_string());
        let output = successful_run(plan).await;
        let expected = format!(
            "{}{}{}",
            if workspace_write { "w" } else { "" },
            if host_read { "r" } else { "" },
            if host_write { "h" } else { "" },
        );
        assert_eq!(output.stdout, expected.as_bytes(), "{capability:?}");
        assert_eq!(fixture.workspace().join("output").exists(), workspace_write);
        assert_eq!(fixture.0.join("output").exists(), host_write);
    }
}

#[tokio::test]
async fn runtime_roots_stay_read_only_and_private_temp_is_removed() {
    let fixture = Fixture::new("macos-runtime");
    let runtime = fixture.0.join("runtime");
    std::fs::create_dir(&runtime).expect("create runtime root");
    std::fs::write(runtime.join("input"), "runtime\n").expect("write runtime input");
    let mut plan = shell_plan(
        &fixture.workspace(),
        r#"
            read -r value < "$RUNTIME/input" && test "$value" = runtime || exit 1
            if (printf changed > "$RUNTIME/input"); then exit 2; fi
            if (printf changed > "$RUNTIME/new"); then exit 3; fi
            printf workspace > output || exit 4
            printf temporary > "$TMPDIR/output" || exit 5
            test "$TMPDIR" = "$TMP" && test "$TMP" = "$TEMP" || exit 6
            printf %s "$TMPDIR"
        "#,
        [SandboxCapability::WorkspaceWrite],
    );
    plan.runtime_roots.push(runtime.clone());
    plan.environment
        .insert("RUNTIME".into(), runtime.as_os_str().to_os_string());
    let output = successful_run(plan).await;
    let private_temp = PathBuf::from(String::from_utf8(output.stdout).expect("private temp path"));
    assert!(!private_temp.exists(), "private temp survives sandbox exit");
    assert_eq!(
        std::fs::read(runtime.join("input")).expect("read runtime input"),
        b"runtime\n"
    );
    assert!(!runtime.join("new").exists());
    assert_eq!(
        std::fs::read(fixture.workspace().join("output")).expect("workspace output"),
        b"workspace"
    );
}

#[tokio::test]
async fn protected_metadata_cannot_be_created_or_replaced() {
    for shape in ["absent", "file", "directory", "symlink"] {
        let fixture = Fixture::new("macos-metadata");
        let workspace = fixture.workspace();
        for name in PROTECTED_METADATA_NAMES {
            let path = workspace.join(name);
            match shape {
                "file" => std::fs::write(&path, "original").expect("write metadata file"),
                "directory" | "symlink" => {
                    let target = if shape == "symlink" {
                        workspace.join(format!("target-{name}"))
                    } else {
                        path.clone()
                    };
                    std::fs::create_dir(&target).expect("create metadata directory");
                    std::fs::write(target.join("original"), "original")
                        .expect("write metadata content");
                    if shape == "symlink" {
                        symlink(&target, &path).expect("link metadata");
                    }
                }
                _ => {}
            }
        }
        let mut plan = shell_plan(
            &workspace,
            r#"
                printf safe > replacement || exit 1
                for name in .git .jj .kraai .agents .codex; do
                    if (printf changed > "$name"); then exit 2; fi
                    if test "$SHAPE" = absent; then
                        if mkdir "$name"; then exit 3; fi
                        if ln -s replacement "$name"; then exit 4; fi
                    else
                        if rm -rf "$name"; then exit 5; fi
                        if mv "$name" "moved-$name"; then exit 6; fi
                    fi
                    if mv replacement "$name"; then exit 7; fi
                    if test "$SHAPE" = directory || test "$SHAPE" = symlink; then
                        if (printf changed > "$name/original"); then exit 8; fi
                        if (printf changed > "$name/new"); then exit 9; fi
                    fi
                    if test "$SHAPE" = symlink; then
                        if (printf changed > "target-$name/original"); then exit 10; fi
                        if (printf changed > "target-$name/new"); then exit 11; fi
                    fi
                done
                printf protected
            "#,
            [SandboxCapability::WorkspaceWrite],
        );
        plan.environment.insert("SHAPE".into(), shape.into());
        let output = successful_run(plan).await;
        assert_eq!(output.stdout, b"protected", "{shape}");
        for name in PROTECTED_METADATA_NAMES {
            let path = workspace.join(name);
            if shape == "absent" {
                assert!(!path.exists());
            } else {
                let original = if shape == "file" {
                    path
                } else {
                    path.join("original")
                };
                assert_eq!(
                    std::fs::read(original).expect("metadata content"),
                    b"original"
                );
            }
        }
    }
}

#[tokio::test]
async fn workspace_alias_preserves_write_and_metadata_boundaries() {
    let fixture = Fixture::new("macos-alias");
    let workspace = fixture.workspace();
    let alias = fixture.0.join("alias");
    symlink(&workspace, &alias).expect("link workspace alias");
    std::fs::create_dir(workspace.join(".git")).expect("create metadata");
    let mut plan = shell_plan(
        &alias,
        r#"
            printf allowed > output || exit 1
            printf allowed > "$REAL/direct" || exit 2
            if (printf forbidden > .git/alias); then exit 3; fi
            if (printf forbidden > "$REAL/.git/direct"); then exit 4; fi
            if (printf forbidden > "$REAL/../outside"); then exit 5; fi
            printf alias
        "#,
        [SandboxCapability::WorkspaceWrite],
    );
    plan.environment
        .insert("REAL".into(), workspace.as_os_str().to_os_string());
    let output = successful_run(plan).await;
    assert_eq!(output.stdout, b"alias");
    assert!(workspace.join("output").exists());
    assert!(workspace.join("direct").exists());
    assert!(!workspace.join(".git/alias").exists());
    assert!(!workspace.join(".git/direct").exists());
    assert!(!fixture.0.join("outside").exists());
}

#[tokio::test]
async fn metadata_write_capability_allows_metadata_changes() {
    let fixture = Fixture::new("macos-metadata-write");
    let output = successful_run(shell_plan(
        &fixture.workspace(),
        r#"
            for name in .git .jj .kraai .agents .codex; do
                mkdir "$name" || exit 1
                printf allowed > "$name/content" || exit 2
            done
            printf metadata
        "#,
        [SandboxCapability::MetadataWrite],
    ))
    .await;
    assert_eq!(output.stdout, b"metadata");
    for name in PROTECTED_METADATA_NAMES {
        assert_eq!(
            std::fs::read(fixture.workspace().join(name).join("content"))
                .expect("metadata write result"),
            b"allowed"
        );
    }
}

#[tokio::test]
async fn overlapping_runtime_roots_preserve_workspace_permissions() {
    for writable in [false, true] {
        for overlap in ["nested", "equal", "ancestor"] {
            let fixture = Fixture::new("macos-runtime-overlap");
            let workspace = fixture.workspace();
            let build = workspace.join("target/debug");
            std::fs::create_dir_all(&build).expect("create build directory");
            std::fs::create_dir(workspace.join(".git")).expect("create metadata");
            let mut plan = shell_plan(
                &workspace,
                r#"
                    if (printf build > target/debug/lock); then printf writable; else printf readonly; fi
                    if (printf forbidden > .git/forbidden); then exit 1; fi
                    if (printf forbidden > ../forbidden); then exit 2; fi
                    exit 0
                "#,
                [if writable {
                    SandboxCapability::WorkspaceWrite
                } else {
                    SandboxCapability::WorkspaceRead
                }],
            );
            plan.runtime_roots.push(match overlap {
                "nested" => build.clone(),
                "equal" => workspace.clone(),
                _ => fixture.0.clone(),
            });
            let output = successful_run(plan).await;
            assert_eq!(
                output.stdout,
                if writable { b"writable" } else { b"readonly" },
                "{overlap}, writable={writable}"
            );
            assert_eq!(build.join("lock").exists(), writable);
            assert!(!workspace.join(".git/forbidden").exists());
            assert!(!fixture.0.join("forbidden").exists());
        }
    }
}

#[tokio::test]
async fn metadata_symlink_target_cannot_escape_protection_by_renaming_ancestors() {
    let fixture = Fixture::new("macos-metadata-ancestor");
    let workspace = fixture.workspace();
    let target = workspace.join("target/nested/metadata");
    std::fs::create_dir_all(&target).expect("create metadata target");
    std::fs::write(target.join("original"), "original").expect("write metadata target");
    symlink(&target, workspace.join(".git")).expect("link metadata target");
    let output = successful_run(shell_plan(
        &workspace,
        r#"
            mkdir ordinary || exit 1
            mv ordinary renamed || exit 2
            if mv target moved; then exit 3; fi
            if mv target/nested target/renamed; then exit 4; fi
            if (printf changed > target/nested/metadata/original); then exit 5; fi
            printf protected
        "#,
        [SandboxCapability::WorkspaceWrite],
    ))
    .await;
    assert_eq!(output.stdout, b"protected");
    assert!(workspace.join("renamed").is_dir());
    assert!(!workspace.join("moved").exists());
    assert!(!workspace.join("target/renamed").exists());
    assert_eq!(
        std::fs::read(target.join("original")).expect("metadata target content"),
        b"original"
    );
}

#[tokio::test]
async fn network_capability_and_private_ipc_paths_are_enforced() {
    for network_allowed in [false, true] {
        let fixture = Fixture::new("macos-network");
        let private_temp = PrivateTempConfig::under("/tmp")
            .reserve()
            .expect("reserve private IPC directory");
        let private_path = private_temp
            .path()
            .expect("private temp path")
            .to_path_buf();
        let allowed_path = private_path.join("allowed.sock");
        let denied_path = private_path.join("denied.sock");
        let private_listener = UnixListener::bind(&allowed_path).expect("private IPC listener");
        let denied_listener = UnixListener::bind(&denied_path).expect("unlisted Unix listener");
        let tcp_listener = TcpListener::bind("127.0.0.1:0").expect("host TCP listener");
        let mut granted = vec![SandboxCapability::WorkspaceRead];
        if network_allowed {
            granted.push(SandboxCapability::Network);
        }
        let executable = std::env::current_exe().expect("test executable");
        let mut plan = LaunchPlan::new(
            executable.clone(),
            fixture.workspace(),
            capabilities(granted),
            Duration::from_secs(10),
        );
        plan.args = [
            "--exact",
            "tests::macos::network_probe_child",
            "--nocapture",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        plan.runtime_roots = vec![
            executable
                .parent()
                .expect("test binary parent")
                .to_path_buf(),
        ];
        if Path::new("/nix/store").exists() {
            plan.runtime_roots.push(PathBuf::from("/nix/store"));
        }
        plan.private_temp = private_temp;
        plan.private_ipc_connect_paths.push(allowed_path.clone());
        plan.environment.insert(
            "KRAAI_MACOS_NETWORK_PROBE".into(),
            if network_allowed {
                "enabled"
            } else {
                "restricted"
            }
            .into(),
        );
        plan.environment.insert(
            "KRAAI_TCP_ADDRESS".into(),
            tcp_listener
                .local_addr()
                .expect("TCP address")
                .to_string()
                .into(),
        );
        plan.environment
            .insert("KRAAI_PRIVATE_SOCKET".into(), allowed_path.into_os_string());
        plan.environment
            .insert("KRAAI_UNLISTED_SOCKET".into(), denied_path.into_os_string());
        successful_run(plan).await;
        private_listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let (mut connection, _) = private_listener.accept().expect("private IPC connected");
        connection
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("IPC read deadline");
        let mut bytes = [0; 3];
        connection
            .read_exact(&mut bytes)
            .expect("private IPC payload");
        assert_eq!(&bytes, b"ipc");
        assert!(!private_path.exists(), "private IPC temp survives exit");
        drop(denied_listener);
    }
}

#[test]
fn network_probe_child() {
    let Ok(mode) = std::env::var("KRAAI_MACOS_NETWORK_PROBE") else {
        return;
    };
    let address: SocketAddr = std::env::var("KRAAI_TCP_ADDRESS")
        .expect("TCP probe address")
        .parse()
        .expect("valid TCP probe address");
    let tcp = TcpStream::connect_timeout(&address, Duration::from_secs(2));
    let unlisted = UnixStream::connect(
        std::env::var_os("KRAAI_UNLISTED_SOCKET").expect("unlisted Unix probe path"),
    );
    if mode == "enabled" {
        tcp.expect("network capability permits TCP");
        unlisted.expect("network capability permits Unix sockets");
    } else {
        for error in [
            tcp.expect_err("restricted sandbox must deny TCP"),
            unlisted.expect_err("restricted sandbox must deny unlisted Unix sockets"),
        ] {
            assert!(
                matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES)),
                "expected sandbox permission denial, got {error}"
            );
        }
    }
    let mut private = UnixStream::connect(
        std::env::var_os("KRAAI_PRIVATE_SOCKET").expect("private Unix probe path"),
    )
    .expect("explicit private IPC path is accessible");
    private
        .write_all(b"ipc")
        .expect("write private IPC payload");
    let (mut left, mut right) = UnixStream::pair().expect("anonymous subprocess IPC pair");
    left.write_all(b"ok").expect("write subprocess IPC");
    let mut bytes = [0; 2];
    right.read_exact(&mut bytes).expect("read subprocess IPC");
    assert_eq!(&bytes, b"ok");
}
