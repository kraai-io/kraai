#[path = "macos/sysctl.rs"]
mod sysctl;

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
async fn timeout_bounds_detached_output_capture() {
    for mode in ["both", "stderr"] {
        super::process::detached_output(mode, SandboxCapability::WorkspaceWrite, false).await;
    }
}

#[tokio::test]
async fn root_access_for_startup_does_not_allow_host_subtree_reads() {
    let fixture = Fixture::new("macos-root-enumeration");
    std::fs::write(fixture.0.join("secret"), b"host data").expect("write host secret");
    for (capability, allowed) in [
        (SandboxCapability::WorkspaceRead, false),
        (SandboxCapability::WorkspaceWrite, false),
        (SandboxCapability::HostRead, true),
        (SandboxCapability::HostWrite, true),
    ] {
        let mut plan = shell_plan(
            &fixture.workspace(),
            "/bin/ls -A / >/dev/null || exit 1; if /bin/cat \"$SECRET\" >/dev/null 2>&1; then printf allowed; else printf denied; fi",
            [capability],
        );
        plan.environment
            .insert("SECRET".into(), fixture.0.join("secret").into_os_string());
        let output = successful_run(plan).await;
        assert_eq!(
            output.stdout,
            if allowed {
                b"allowed".as_slice()
            } else {
                b"denied".as_slice()
            },
            "{capability:?}"
        );
    }
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
async fn workspace_alias_preserves_workspace_boundaries() {
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
            printf allowed > .git/alias || exit 3
            printf allowed > "$REAL/.git/direct" || exit 4
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
    assert!(workspace.join(".git/alias").exists());
    assert!(workspace.join(".git/direct").exists());
    assert!(!fixture.0.join("outside").exists());
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
            assert!(!fixture.0.join("forbidden").exists());
        }
    }
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
        connection.set_nonblocking(true).expect("nonblocking IPC");
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

#[tokio::test]
async fn sandbox_process_groups_stop_on_every_completion_path() {
    for mode in ["exit", "timeout", "cancel", "drop"] {
        super::process::assert_process_group_stops(mode, SandboxCapability::WorkspaceWrite).await;
    }
}
