use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kraai_types::{SandboxCapabilities, SandboxCapability};

use super::profile::build_args;
use crate::config::LaunchPlan;
use crate::temp_dir::PrivateTempDir;

fn fixture(capabilities: &[SandboxCapability]) -> (PrivateTempDir, LaunchPlan, PathBuf) {
    let root = PrivateTempDir::create(None).expect("create fixture");
    let workspace = root.path().join("workspace");
    let private_temp = root.path().join("private");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::fs::create_dir(&private_temp).expect("create private temp");
    let plan = LaunchPlan::new(
        PathBuf::from("/bin/sh"),
        workspace,
        SandboxCapabilities::new(capabilities.iter().copied()).expect("valid capabilities"),
        Duration::from_secs(5),
    );
    (root, plan, private_temp)
}

fn source(args: &[OsString]) -> &str {
    args[1].to_str().expect("UTF-8 policy")
}

fn parameters_for(args: &[OsString], path: &Path) -> Vec<String> {
    let suffix = format!("={}", path.display());
    args.iter()
        .filter_map(|arg| arg.to_str())
        .filter_map(|arg| arg.strip_prefix("-D"))
        .filter_map(|arg| arg.strip_suffix(&suffix))
        .map(|name| format!("(param \"{name}\")"))
        .collect()
}

#[test]
fn paths_are_parameters_and_command_arguments_are_preserved() {
    let (root, mut plan, private_temp) = fixture(&[SandboxCapability::WorkspaceWrite]);
    let workspace = root.path().join("workspace \" )\n(allow default) ; ü");
    std::fs::rename(&plan.workspace_root, &workspace).expect("rename workspace");
    plan.workspace_root = workspace.clone();
    plan.args = vec!["-c".into(), "printf '%s' '$HOME'".into()];

    let args = build_args(&plan, &private_temp).expect("build policy");

    assert!(source(&args).starts_with("(version 1)\n(deny default)"));
    assert!(!source(&args).contains("(allow default)"));
    assert!(!parameters_for(&args, &workspace.canonicalize().expect("canonical path")).is_empty());
    assert_eq!(
        &args[args.len() - 4..],
        [
            OsString::from("--"),
            plan.executable.into_os_string(),
            plan.args[0].clone(),
            plan.args[1].clone()
        ]
    );
}

#[test]
fn host_access_requires_the_corresponding_capability() {
    for capability in [
        SandboxCapability::WorkspaceRead,
        SandboxCapability::HostRead,
        SandboxCapability::HostWrite,
    ] {
        let (_root, plan, private_temp) = fixture(&[capability]);
        let args = build_args(&plan, &private_temp).expect("build policy");
        assert_eq!(
            source(&args).contains("(allow file-read* file-map-executable)"),
            capability != SandboxCapability::WorkspaceRead
        );
        assert_eq!(
            source(&args).contains("(allow file-write*)"),
            capability == SandboxCapability::HostWrite
        );
        assert!(!source(&args).contains("(allow network*)"));
    }
}

#[test]
fn metadata_paths_are_protected_even_before_creation() {
    let (_root, plan, private_temp) = fixture(&[SandboxCapability::WorkspaceWrite]);
    let args = build_args(&plan, &private_temp).expect("build policy");
    let workspace = plan
        .workspace_root
        .canonicalize()
        .expect("canonical workspace");
    for name in crate::platform::PROTECTED_METADATA_NAMES {
        let parameters = parameters_for(&args, &workspace.join(name));
        assert!(!parameters.is_empty());
        for parameter in parameters {
            assert!(source(&args).contains(&format!(
                "(deny file-write* (require-any (literal {parameter}) (subpath {parameter})))"
            )));
        }
    }
    let parameters = parameters_for(&args, &workspace);
    assert!(parameters.iter().any(|parameter| source(&args).contains(&format!("(deny file-write-unlink (require-all (literal {parameter}) (vnode-type DIRECTORY)))"))));
}

#[test]
fn metadata_write_removes_metadata_restrictions() {
    let (_root, plan, private_temp) = fixture(&[SandboxCapability::MetadataWrite]);
    let args = build_args(&plan, &private_temp).expect("build policy");
    assert!(!source(&args).contains("(deny file-write*"));
    assert!(source(&args).contains("(allow file-write* (require-any"));
}

#[test]
fn runtime_files_are_literal_and_stay_read_only_with_host_write() {
    let (root, mut plan, private_temp) = fixture(&[SandboxCapability::HostWrite]);
    let runtime = root.path().join("runtime");
    std::fs::write(&runtime, "runtime").expect("create runtime file");
    plan.runtime_roots.push(runtime.clone());
    let args = build_args(&plan, &private_temp).expect("build policy");
    let parameters = parameters_for(&args, &runtime.canonicalize().expect("canonical runtime"));
    assert!(!parameters.is_empty());
    for parameter in parameters {
        assert!(source(&args).contains(&format!(
            "(deny file-write* (require-all (literal {parameter}) (require-not"
        )));
        assert!(!source(&args).contains(&format!("(subpath {parameter})")));
    }
}

#[test]
fn runtime_root_inside_workspace_does_not_override_workspace_write() {
    let (_root, mut plan, private_temp) = fixture(&[SandboxCapability::MetadataWrite]);
    let runtime = plan.workspace_root.join("target");
    std::fs::create_dir(&runtime).expect("create runtime directory");
    plan.runtime_roots.push(runtime);
    let args = build_args(&plan, &private_temp).expect("build policy");
    assert!(!source(&args).contains("(deny file-write*"));
}

#[test]
fn network_access_is_explicit_and_private_ipc_is_limited_to_one_path() {
    let (_root, mut plan, private_temp) = fixture(&[SandboxCapability::WorkspaceRead]);
    let socket = private_temp.join("host.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind socket");
    plan.private_ipc_connect_paths.push(socket.clone());
    let args = build_args(&plan, &private_temp).expect("build policy");
    assert!(!source(&args).contains("(allow network*)"));
    assert!(source(&args).contains("(allow system-socket (socket-domain AF_UNIX))"));
    let parameters = parameters_for(&args, &socket.canonicalize().expect("canonical socket"));
    assert_eq!(parameters.len(), 1);
    assert!(source(&args).contains(&format!(
        "(allow network-outbound (remote unix-socket (literal {})))",
        parameters[0]
    )));
    assert!(!source(&args).contains("network-bind"));

    plan.capabilities =
        SandboxCapabilities::new([SandboxCapability::WorkspaceRead, SandboxCapability::Network])
            .expect("network capabilities");
    let args = build_args(&plan, &private_temp).expect("network policy");
    assert!(source(&args).contains("(allow network*)"));
    assert!(source(&args).contains("com.apple.SystemConfiguration.DNSConfiguration"));
}

#[test]
fn private_ipc_outside_private_temp_is_rejected() {
    let (_root, mut plan, private_temp) = fixture(&[SandboxCapability::WorkspaceRead]);
    let socket = plan.workspace_root.join("host.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind socket");
    plan.private_ipc_connect_paths.push(socket);
    assert!(build_args(&plan, &private_temp).is_err());
}

#[test]
fn symlinked_workspace_and_metadata_resolve_to_their_targets() {
    let (root, mut plan, private_temp) = fixture(&[SandboxCapability::WorkspaceWrite]);
    let target = plan.workspace_root.join("metadata-target");
    std::fs::create_dir(&target).expect("create target");
    std::os::unix::fs::symlink(&target, plan.workspace_root.join(".git"))
        .expect("symlink metadata");
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&plan.workspace_root, &alias).expect("symlink workspace");
    plan.workspace_root = alias;
    let args = build_args(&plan, &private_temp).expect("build policy");
    let parameters = parameters_for(&args, &target.canonicalize().expect("canonical target"));
    assert!(!parameters.is_empty());
    assert!(
        parameters
            .iter()
            .any(|parameter| source(&args).contains(&format!(
                "(deny file-write* (require-any (literal {parameter}) (subpath {parameter})))"
            )))
    );
}

#[test]
fn dangling_metadata_symlinks_fail_closed() {
    let (_root, plan, private_temp) = fixture(&[SandboxCapability::WorkspaceWrite]);
    std::os::unix::fs::symlink("missing", plan.workspace_root.join(".git"))
        .expect("symlink metadata");
    assert!(build_args(&plan, &private_temp).is_err());
}

#[test]
fn non_utf8_paths_are_rejected_instead_of_lossily_granted() {
    use std::os::unix::ffi::OsStringExt;

    let (root, mut plan, private_temp) = fixture(&[SandboxCapability::WorkspaceRead]);
    let workspace = root.path().join(OsString::from_vec(vec![b'w', 0xff]));
    std::fs::create_dir(&workspace).expect("create non-UTF-8 workspace");
    plan.workspace_root = workspace;
    assert!(build_args(&plan, &private_temp).is_err());
}

#[test]
fn sysctl_access_is_limited_to_runtime_information() {
    for capabilities in [
        vec![SandboxCapability::WorkspaceRead],
        vec![SandboxCapability::HostWrite, SandboxCapability::Network],
    ] {
        let (_root, plan, private_temp) = fixture(&capabilities);
        let args = build_args(&plan, &private_temp).expect("build policy");
        let policy = source(&args);
        assert!(!policy.contains("(allow sysctl-read)"));
        assert!(!policy.contains("kern.procargs"));
        assert!(!policy.contains("(sysctl-name-prefix \"kern.\")"));
        for name in ["kern.argmax", "hw.ncpu", "machdep.cpu.brand_string"] {
            assert!(policy.contains(&format!("(sysctl-name \"{name}\")")));
        }
    }
}
