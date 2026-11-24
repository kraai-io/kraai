use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kraai_execution_types::{SandboxCapabilities, SandboxCapability};

use super::profile::build_args;
use crate::config::LaunchPlan;
use crate::temp_dir::PrivateTempDir;
use crate::tests::git_worktree::LinkedWorktree;

fn fixture(capabilities: &[SandboxCapability]) -> (PrivateTempDir, LaunchPlan, PathBuf) {
    let root = PrivateTempDir::create(None).expect("create fixture");
    let workspace = root.path().join("workspace");
    let private_temp = root.path().join("private");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::fs::create_dir(&private_temp).expect("create private temp");
    let plan = LaunchPlan::new(
        PathBuf::from("/bin/sh"),
        workspace,
        SandboxCapabilities::new(capabilities.iter().copied()),
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

    let args = build_args(&plan, &private_temp).expect("build policy").args;

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
        let args = build_args(&plan, &private_temp).expect("build policy").args;
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
fn workspace_write_allows_all_workspace_paths() {
    let (_root, plan, private_temp) = fixture(&[SandboxCapability::WorkspaceWrite]);
    let args = build_args(&plan, &private_temp).expect("build policy").args;
    assert!(!source(&args).contains("(deny file-write* (require-all"));
    assert!(source(&args).contains("(allow file-write* (require-any"));
}

#[test]
fn runtime_files_are_literal_and_stay_read_only_with_host_write() {
    let (root, mut plan, private_temp) = fixture(&[SandboxCapability::HostWrite]);
    let runtime = root.path().join("runtime");
    std::fs::write(&runtime, "runtime").expect("create runtime file");
    plan.runtime_roots.push(runtime.clone());
    let args = build_args(&plan, &private_temp).expect("build policy").args;
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
    let (_root, mut plan, private_temp) = fixture(&[SandboxCapability::WorkspaceWrite]);
    let runtime = plan.workspace_root.join("target");
    std::fs::create_dir(&runtime).expect("create runtime directory");
    plan.runtime_roots.push(runtime);
    let args = build_args(&plan, &private_temp).expect("build policy").args;
    assert!(!source(&args).contains("(deny file-write* (require-all"));
}

#[test]
fn network_access_is_explicit_and_private_ipc_is_limited_to_one_path() {
    let (_root, mut plan, private_temp) = fixture(&[SandboxCapability::WorkspaceRead]);
    let socket = private_temp.join("host.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind socket");
    plan.private_ipc_connect_paths.push(socket.clone());
    let args = build_args(&plan, &private_temp).expect("build policy").args;
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
        SandboxCapabilities::new([SandboxCapability::WorkspaceRead, SandboxCapability::Network]);
    let args = build_args(&plan, &private_temp)
        .expect("network policy")
        .args;
    assert!(!source(&args).contains("(allow network*)"));
    assert!(source(&args).contains("(allow network-outbound (remote ip \"*:*\"))"));
    let temp_parameters =
        parameters_for(&args, &private_temp.canonicalize().expect("canonical temp"));
    assert!(
        temp_parameters
            .iter()
            .any(|parameter| source(&args).contains(&format!(
                "(allow network-outbound (remote unix-socket (subpath {parameter})))"
            )))
    );
    let workspace_parameters = parameters_for(
        &args,
        &plan
            .workspace_root
            .canonicalize()
            .expect("canonical workspace"),
    );
    assert!(!workspace_parameters.iter().any(|parameter| {
        source(&args).contains(&format!("(remote unix-socket (subpath {parameter}))"))
    }));
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
fn non_utf8_paths_are_rejected_instead_of_lossily_granted() {
    use std::os::unix::ffi::OsStringExt;

    let (root, mut plan, private_temp) = fixture(&[SandboxCapability::WorkspaceRead]);
    let workspace = root.path().join(OsString::from_vec(vec![b'w', 0xff]));
    if let Err(error) = std::fs::create_dir(&workspace) {
        assert_eq!(error.raw_os_error(), Some(libc::EILSEQ));
    }
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
        let args = build_args(&plan, &private_temp).expect("build policy").args;
        let policy = source(&args);
        assert!(!policy.contains("(allow sysctl-read)"));
        assert!(policy.contains("(deny sysctl-read (sysctl-name-prefix \"kern.procargs\"))"));
        assert!(!policy.contains("(sysctl-name-prefix \"kern.\")"));
        for name in ["kern.argmax", "hw.ncpu", "machdep.cpu.brand_string"] {
            assert!(policy.contains(&format!("(sysctl-name \"{name}\")")));
        }
    }
}

#[test]
fn workspace_write_allows_linked_worktree_common_dir_over_runtime_roots() {
    for (capability, writable) in [
        (SandboxCapability::WorkspaceWrite, true),
        (SandboxCapability::WorkspaceRead, false),
    ] {
        let worktree = LinkedWorktree::new();
        let private_temp = PrivateTempDir::create(None).expect("create private temp");
        let mut plan = LaunchPlan::new(
            PathBuf::from("/bin/sh"),
            worktree.workspace.clone(),
            SandboxCapabilities::new([capability]),
            Duration::from_secs(5),
        );
        plan.runtime_roots.push(
            worktree
                .workspace
                .parent()
                .expect("fixture base")
                .to_path_buf(),
        );
        let args = build_args(&plan, private_temp.path())
            .expect("build policy")
            .args;
        let policy = source(&args);
        let parameters = parameters_for(&args, &worktree.common_dir);
        if !writable {
            assert!(parameters.is_empty());
            continue;
        }
        let filter = format!("(require-any (literal {0}) (subpath {0}))", parameters[0]);
        assert!(policy.contains(&format!("(allow file-read* file-write* {filter})")));
        assert!(policy.contains(&format!("(require-not {filter})")));
    }
}

#[test]
fn kraai_configuration_is_write_protected_only_for_writable_workspaces() {
    let (_root, plan, private_temp) = fixture(&[SandboxCapability::WorkspaceRead]);
    let args = build_args(&plan, &private_temp)
        .expect("read-only policy")
        .args;
    let workspace = plan
        .workspace_root
        .canonicalize()
        .expect("canonical workspace");
    assert!(parameters_for(&args, &workspace.join(".kraai")).is_empty());
    assert!(parameters_for(&args, &workspace.join("AGENTS.md")).is_empty());

    let (_root, plan, private_temp) = fixture(&[SandboxCapability::WorkspaceWrite]);
    let workspace = plan
        .workspace_root
        .canonicalize()
        .expect("canonical workspace");
    std::fs::create_dir_all(workspace.join("shared")).expect("create shared");
    std::fs::write(workspace.join("shared/agents.toml"), "").expect("write profiles");
    std::fs::create_dir(workspace.join(".kraai")).expect("create config");
    std::os::unix::fs::symlink(
        "../shared/agents.toml",
        workspace.join(".kraai/agents.toml"),
    )
    .expect("link profiles");
    std::fs::write(workspace.join("AGENTS.md"), "").expect("write instructions");
    let args = build_args(&plan, &private_temp)
        .expect("writable policy")
        .args;
    for name in [".kraai", "AGENTS.md"] {
        let entry = parameters_for(&args, &workspace.join(name));
        assert!(
            entry
                .iter()
                .any(|parameter| source(&args).contains(&format!(
                    "(deny file-write* (require-any (literal {parameter}) (subpath {parameter})))"
                ))),
            "{name} must be protected"
        );
    }
    let profiles = parameters_for(&args, &workspace.join("shared/agents.toml"));
    assert!(profiles.iter().any(|parameter| {
        source(&args).contains(&format!("(deny file-write* (literal {parameter}))"))
    }));
}

#[test]
fn each_run_gets_a_unique_mach_lookup_marker() {
    let (_root, plan, private_temp) = fixture(&[SandboxCapability::WorkspaceWrite]);
    let first = build_args(&plan, &private_temp).expect("first policy");
    let second = build_args(&plan, &private_temp).expect("second policy");
    assert_ne!(first.run_marker, second.run_marker);
    for command in [&first, &second] {
        let suffix = command
            .run_marker
            .strip_prefix("dev.kraai.run.")
            .expect("marker prefix");
        assert_eq!(suffix.len(), 32);
        assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(command.args.contains(&OsString::from(format!(
            "-DKRAAI_RUN={}",
            command.run_marker
        ))));
        let policy = source(&command.args);
        assert!(policy.contains("(allow mach-lookup (global-name (param \"KRAAI_RUN\")))"));
        assert!(!policy.contains(&command.run_marker));
        assert!(policy.contains("(deny job-creation lsopen appleevent-send)"));
    }
}

#[test]
fn denied_paths_override_host_access_and_keep_grants_inside_them() {
    let (root, mut plan, private_temp) = fixture(&[SandboxCapability::HostWrite]);
    let storage = root.path().join("storage");
    let output = storage.join("script-output/session");
    std::fs::create_dir_all(&output).expect("create output");
    let config = root.path().join("providers.toml");
    std::fs::write(&config, "api_key = 'secret'").expect("write config");
    let missing = root.path().join("missing/mcp.toml");
    plan.runtime_roots.push(output.clone());
    plan.denied_paths = vec![missing, config.clone(), storage.clone()];
    let args = build_args(&plan, &private_temp).expect("build policy").args;
    let policy = source(&args);
    let canonical = |path: &Path| path.canonicalize().expect("canonical path");
    let rule_at = |path: &Path, rule: &dyn Fn(&str) -> String| {
        parameters_for(&args, path)
            .iter()
            .find_map(|parameter| policy.find(&rule(parameter)))
    };

    let storage_denied = rule_at(&canonical(&storage), &|parameter| {
        format!("(deny file-read* file-write* (require-any (literal {parameter}) (subpath {parameter})))")
    })
    .expect("storage deny");
    let output_allowed = parameters_for(&args, &canonical(&output))
        .iter()
        .filter_map(|parameter| {
            policy.rfind(&format!(
                "(allow file-read* file-map-executable (require-any (literal {parameter})"
            ))
        })
        .max()
        .expect("runtime root allow");
    assert!(output_allowed > storage_denied);
    assert!(
        rule_at(&canonical(&config), &|parameter| format!(
            "(deny file-read* file-write* (literal {parameter}))"
        ))
        .is_some()
    );
    let missing = canonical(root.path()).join("missing/mcp.toml");
    assert!(
        rule_at(&missing, &|parameter| format!(
            "(deny file-read* file-write* (require-any (literal {parameter}) (subpath {parameter})))"
        ))
        .is_some()
    );
    let temp_grants = parameters_for(&args, &canonical(&private_temp))
        .iter()
        .map(|parameter| {
            policy
                .matches(&format!(
                    "(allow file-read* file-map-executable file-write* (require-any (literal {parameter})"
                ))
                .count()
        })
        .sum::<usize>();
    assert_eq!(temp_grants, 1);
}
