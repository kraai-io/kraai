#![expect(
    clippy::expect_used,
    reason = "integration tests assert real operating system permission decisions"
)]

use std::path::PathBuf;
use std::time::Duration;

use kraai_sandbox::{LaunchPlan, PrivateTempConfig, Termination};
use kraai_types::{SandboxCapabilities, SandboxCapability};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _temp: PrivateTempConfig,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = PrivateTempConfig::default()
            .reserve()
            .expect("reserve fixture");
        let root = temp
            .path()
            .expect("fixture path")
            .canonicalize()
            .expect("resolve fixture");
        for directory in [
            "workspace/.git",
            "workspace/gitdir",
            "workspace/common",
            "runtime",
        ] {
            std::fs::create_dir_all(root.join(directory)).expect("create fixture directory");
        }
        for name in [
            "workspace/input",
            "workspace/.git/config",
            "workspace/gitdir/config",
            "workspace/common/config",
            "secret",
            "runtime/data",
        ] {
            std::fs::write(root.join(name), "original").expect("write fixture");
        }
        Self { _temp: temp, root }
    }

    fn plan(&self, capabilities: &[SandboxCapability], linked: bool) -> LaunchPlan {
        let executable = std::env::current_exe().expect("test executable");
        let mut plan = LaunchPlan::new(
            executable.clone(),
            self.root.join("workspace"),
            SandboxCapabilities::new(capabilities.iter().copied()).expect("capabilities"),
            Duration::from_secs(30),
        );
        plan.runtime_roots
            .extend([executable, self.root.join("runtime")]);
        #[cfg(unix)]
        for root in ["/nix/store", "/lib", "/lib64", "/usr/lib"] {
            let root = PathBuf::from(root);
            if root.exists() {
                plan.runtime_roots.push(root);
            }
        }
        plan.args(["--exact", "permission_probe", "--nocapture"]);
        plan.environment
            .insert("KRAAI_PERMISSION_PROBE".into(), "1".into());
        plan.environment
            .insert("KRAAI_FIXTURE".into(), self.root.clone().into_os_string());
        for (name, capability) in [
            ("WRITE", SandboxCapability::WorkspaceWrite),
            ("METADATA", SandboxCapability::MetadataWrite),
            ("HOST_READ", SandboxCapability::HostRead),
            ("HOST_WRITE", SandboxCapability::HostWrite),
            ("NETWORK", SandboxCapability::Network),
            ("UNSANDBOXED", SandboxCapability::NoSandbox),
        ] {
            plan.environment.insert(
                name.into(),
                if plan.capabilities.contains(capability) {
                    "1"
                } else {
                    "0"
                }
                .into(),
            );
        }
        plan.environment
            .insert("LINKED".into(), if linked { "1" } else { "0" }.into());
        plan
    }
}

fn enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1")
}

#[test]
fn permission_probe() {
    if !enabled("KRAAI_PERMISSION_PROBE") {
        return;
    }
    let root = PathBuf::from(std::env::var_os("KRAAI_FIXTURE").expect("fixture root"));
    assert_eq!(
        std::fs::read_to_string("input").expect("workspace read"),
        "original"
    );
    assert_eq!(
        std::fs::write("created", "write").is_ok(),
        enabled("WRITE"),
        "workspace write"
    );
    assert_eq!(
        std::fs::read(root.join("secret")).is_ok(),
        enabled("HOST_READ"),
        "host read"
    );
    let wrote_host_path = std::fs::write(root.join("secret"), "write").is_ok();
    if enabled("HOST_READ") {
        assert_eq!(wrote_host_path, enabled("HOST_WRITE"), "host write");
    }
    let metadata = if enabled("LINKED") {
        "gitdir/config"
    } else {
        ".git/config"
    };
    assert_eq!(
        std::fs::write(metadata, "write").is_ok(),
        enabled("METADATA"),
        "metadata write"
    );
    if enabled("LINKED") {
        assert_eq!(
            std::fs::write("common/config", "write").is_ok(),
            enabled("METADATA"),
            "common metadata write"
        );
    }
    assert!(
        std::fs::read(root.join("runtime/data")).is_ok(),
        "runtime read"
    );
    assert_eq!(
        std::fs::write(root.join("runtime/data"), "write").is_ok(),
        enabled("UNSANDBOXED"),
        "runtime roots remain read-only even with host-write"
    );
    std::fs::write(std::env::temp_dir().join("scratch"), "scratch")
        .expect("private temporary write");
    assert_eq!(
        std::net::TcpListener::bind("0.0.0.0:0").is_ok(),
        enabled("NETWORK"),
        "network capability"
    );
    let address = std::env::var("KRAAI_NETWORK_ADDRESS")
        .expect("network address")
        .parse()
        .expect("parse network address");
    assert_eq!(
        std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_ok(),
        enabled("NETWORK"),
        "network capability permits connecting to host services"
    );
}

async fn verify(capabilities: &[SandboxCapability], linked: bool) {
    let fixture = Fixture::new();
    if linked {
        std::fs::remove_dir_all(fixture.root.join("workspace/.git")).expect("remove git directory");
        std::fs::write(fixture.root.join("workspace/.git"), "gitdir: gitdir\n")
            .expect("git pointer");
        std::fs::write(
            fixture.root.join("workspace/gitdir/commondir"),
            "../common\n",
        )
        .expect("common pointer");
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("host listener");
    let mut plan = fixture.plan(capabilities, linked);
    plan.environment.insert(
        "KRAAI_NETWORK_ADDRESS".into(),
        listener
            .local_addr()
            .expect("listener address")
            .to_string()
            .into(),
    );
    let workspace_read = plan.capabilities.contains(SandboxCapability::WorkspaceRead);
    let result = kraai_sandbox::run(plan, CancellationToken::new()).await;
    if !workspace_read {
        assert!(matches!(
            result,
            Err(kraai_sandbox::SandboxError::WorkspaceReadRequired)
        ));
        return;
    }
    let output = result.expect("sandbox must launch");
    assert_eq!(
        output.termination,
        Termination::Exited { code: Some(0) },
        "capabilities={capabilities:?}, linked={linked}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let capabilities =
        SandboxCapabilities::new(capabilities.iter().copied()).expect("capabilities");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("secret")).expect("host file"),
        if capabilities.contains(SandboxCapability::HostWrite) {
            "write"
        } else {
            "original"
        }
    );
}

macro_rules! capability_cases {
    ($($name:ident: [$($capability:ident),*];)*) => {
        mod capability_matrix {
            use super::*;
            $(mod $name {
                use super::*;
                #[tokio::test]
                async fn offline() {
                    verify(&[$(SandboxCapability::$capability),*], false).await;
                }
                #[tokio::test]
                async fn online() {
                    verify(&[$(SandboxCapability::$capability,)* SandboxCapability::Network], false).await;
                }
            })*
            #[tokio::test]
            async fn unsandboxed() {
                verify(&[SandboxCapability::NoSandbox], false).await;
            }
        }
    };
}

capability_cases! {
    missing_workspace: [];
    workspace_read: [WorkspaceRead];
    workspace_write: [WorkspaceWrite];
    metadata_write: [MetadataWrite];
    host_read: [HostRead];
    host_read_workspace_write: [HostRead, WorkspaceWrite];
    host_read_metadata_write: [HostRead, MetadataWrite];
    host_write: [HostWrite];
}

mod linked_metadata_obeys_the_same_capabilities {
    use super::*;

    #[tokio::test]
    async fn workspace_write() {
        verify(&[SandboxCapability::WorkspaceWrite], true).await;
    }

    #[tokio::test]
    async fn metadata_write() {
        verify(&[SandboxCapability::MetadataWrite], true).await;
    }

    #[tokio::test]
    async fn host_write() {
        verify(&[SandboxCapability::HostWrite], true).await;
    }
}
