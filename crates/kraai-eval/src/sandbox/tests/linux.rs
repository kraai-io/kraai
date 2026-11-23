use crate::sandbox::*;
use std::fs;

#[test]
fn sandbox_keeps_host_shell_aliases_executable_for_native_runners() {
    let Some(git) = find_program("git") else {
        return;
    };
    if find_program("bwrap").is_none() || !Path::new("/bin/sh").is_file() {
        return;
    }
    let workspace = std::env::temp_dir().join(format!(
        "kraai-eval-sandbox-shell-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir_all(workspace.join(".git")).unwrap();
    let outcome = run_sandboxed(SandboxRequest {
        command: vec![
            git.to_string_lossy().into_owned(),
            String::from("-c"),
            String::from(
                "alias.eval-shell=!/bin/sh -c 'find . -maxdepth 0 -print0 | xargs -0 printf %s'",
            ),
            String::from("eval-shell"),
        ],
        workspace: workspace.clone(),
        timeout: kraai_test_support::HANG_TIMEOUT,
        network: NetworkPolicy::Disabled,
        proxy_bridge: None,
        environment: BTreeMap::new(),
        extra_programs: ["find", "xargs", "ls"]
            .into_iter()
            .map(|name| find_program(name).ok_or_else(|| color_eyre::eyre::eyre!("missing {name}")))
            .collect::<Result<Vec<_>>>()
            .unwrap(),
        cargo_home: None,
        metrics_output: None,
        script_executions_dir: None,
        resource_limits: None,
    })
    .unwrap();
    fs::remove_dir_all(workspace).unwrap();
    assert!(
        outcome.success() && outcome.stdout == b".",
        "native runner could not use host shell alias: {}",
        String::from_utf8_lossy(&outcome.stderr)
    );
}

#[test]
fn sandbox_clears_host_environment_and_passes_only_explicit_values() {
    let Some(shell) = find_program("sh") else {
        return;
    };
    if find_program("bwrap").is_none() {
        return;
    }
    let workspace =
        std::env::temp_dir().join(format!("kraai-eval-sandbox-env-{}", ulid::Ulid::generate()));
    fs::create_dir_all(workspace.join(".git")).unwrap();
    let outcome = run_sandboxed(SandboxRequest {
        command: vec![
            shell.to_string_lossy().into_owned(),
            String::from("-c"),
            String::from("[ \"$EVAL_PROXY_TOKEN\" = short-lived ] && [ -z \"${CARGO_HOME:-}\" ]"),
        ],
        workspace: workspace.clone(),
        timeout: kraai_test_support::HANG_TIMEOUT,
        network: NetworkPolicy::Disabled,
        proxy_bridge: None,
        environment: BTreeMap::from([(
            String::from("EVAL_PROXY_TOKEN"),
            String::from("short-lived"),
        )]),
        extra_programs: Vec::new(),
        cargo_home: None,
        metrics_output: None,
        script_executions_dir: None,
        resource_limits: None,
    })
    .unwrap();
    assert!(outcome.success(), "sandbox did not isolate environment");
    assert!(
        !outcome.command.iter().any(|item| item == "short-lived"),
        "sandbox credential leaked into recorded command"
    );
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn sandbox_mounts_a_readable_resolved_ca_bundle() {
    let Some(shell) = find_program("sh") else {
        return;
    };
    if find_program("bwrap").is_none() || host_ca_bundle().is_none() {
        return;
    }
    let workspace =
        std::env::temp_dir().join(format!("kraai-eval-sandbox-ca-{}", ulid::Ulid::generate()));
    fs::create_dir_all(workspace.join(".git")).unwrap();
    let outcome = run_sandboxed(SandboxRequest {
        command: vec![
            shell.to_string_lossy().into_owned(),
            String::from("-c"),
            String::from(
                "[ \"$SSL_CERT_FILE\" = /run/kraai-eval/ca-bundle.crt ] && [ \"$NIX_SSL_CERT_FILE\" = \"$SSL_CERT_FILE\" ] && [ -r \"$SSL_CERT_FILE\" ]",
            ),
        ],
        workspace: workspace.clone(),
        timeout: kraai_test_support::HANG_TIMEOUT,
        network: NetworkPolicy::Disabled,
        proxy_bridge: None,
        environment: BTreeMap::new(),
        extra_programs: Vec::new(),
        cargo_home: None,
        metrics_output: None,
        script_executions_dir: None,
        resource_limits: None,
    }).unwrap();
    assert!(outcome.success(), "sandbox CA bundle was not readable");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn sandbox_persists_script_execution_artifacts() {
    let Some(shell) = find_program("sh") else {
        return;
    };
    if find_program("bwrap").is_none() {
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-script-output-{}",
        ulid::Ulid::generate()
    ));
    let workspace = root.join("workspace");
    let executions = root.join("script-executions");
    fs::create_dir_all(workspace.join(".git")).unwrap();
    fs::create_dir_all(&executions).unwrap();
    let outcome = run_sandboxed(SandboxRequest {
        command: vec![
            shell.to_string_lossy().into_owned(),
            String::from("-c"),
            String::from(
                "printf stdout > \"$HOME/.kraai/data/executions/stdout.bin\" && printf stderr > \"$HOME/.kraai/data/executions/stderr.bin\"",
            ),
        ],
        workspace,
        timeout: kraai_test_support::HANG_TIMEOUT,
        network: NetworkPolicy::Disabled,
        proxy_bridge: None,
        environment: BTreeMap::new(),
        extra_programs: Vec::new(),
        cargo_home: None,
        metrics_output: None,
        script_executions_dir: Some(executions.clone()),
        resource_limits: None,
    }).unwrap();
    assert!(
        outcome.success(),
        "sandbox command failed: exit_code={:?}, timed_out={}, stdout={}, stderr={}",
        outcome.exit_code,
        outcome.timed_out,
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(
        fs::read(executions.join("stdout.bin")).unwrap() == b"stdout",
        "sandbox stdout artifact was not persisted"
    );
    assert!(
        fs::read(executions.join("stderr.bin")).unwrap() == b"stderr",
        "sandbox stderr artifact was not persisted"
    );
    fs::remove_dir_all(root).unwrap();
}

fn runner_args(
    network: NetworkPolicy,
    proxy_bridge: Option<BridgeMount>,
) -> Option<Result<Vec<String>>> {
    let shell = find_program("sh")?;
    find_program("bwrap")?;
    let workspace = std::env::temp_dir().join(format!(
        "kraai-eval-sandbox-network-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir_all(&workspace).unwrap();
    let args = build_args(&SandboxRequest {
        command: vec![
            shell.to_string_lossy().into_owned(),
            String::from("-c"),
            String::from("true"),
        ],
        workspace: workspace.clone(),
        timeout: kraai_test_support::HANG_TIMEOUT,
        network,
        proxy_bridge,
        environment: BTreeMap::new(),
        extra_programs: Vec::new(),
        cargo_home: None,
        metrics_output: None,
        script_executions_dir: None,
        resource_limits: None,
    });
    fs::remove_dir_all(workspace).unwrap();
    Some(args)
}

fn sandboxed_command(args: &[String]) -> &[String] {
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .expect("bwrap arguments have no command");
    args.get(separator + 1..).unwrap_or_default()
}

#[test]
fn disabled_network_reaches_only_the_model_proxy_bridge() {
    let Some(shell) = find_program("sh") else {
        return;
    };
    let bridge = ProxyBridge::create(&shell).unwrap();
    let Some(args) = runner_args(NetworkPolicy::Disabled, Some(bridge.mount())) else {
        return;
    };
    let args = args.unwrap();
    let binds = bridge.mount().bind_args().unwrap();
    let prefix = BridgeMount::command_prefix();
    let command = sandboxed_command(&args);
    assert!(
        args.iter().any(|arg| arg == "--unshare-net"),
        "the runner shares the host network"
    );
    assert!(
        !args.iter().any(|arg| arg == "/etc/resolv.conf"),
        "the offline runner received host name resolution"
    );
    assert!(
        args.windows(binds.len()).any(|window| window == binds),
        "the bridge program or proxy socket directory is not mounted"
    );
    assert!(
        command.starts_with(&prefix)
            && command
                .get(prefix.len())
                .is_some_and(|program| program.starts_with("/runner/")),
        "the runner does not start behind the proxy bridge: {command:?}"
    );
}

#[test]
fn enabled_network_runs_the_runner_on_the_host_network() {
    let Some(args) = runner_args(NetworkPolicy::Enabled, None) else {
        return;
    };
    let args = args.unwrap();
    let command = sandboxed_command(&args);
    assert!(
        !args.iter().any(|arg| arg == "--unshare-net"),
        "the networked runner lost host network access"
    );
    assert!(
        command
            .first()
            .is_some_and(|program| program.starts_with("/runner/")),
        "the networked runner was wrapped: {command:?}"
    );
    let Some(shell) = find_program("sh") else {
        return;
    };
    let bridge = ProxyBridge::create(&shell).unwrap();
    let Some(bridged) = runner_args(NetworkPolicy::Enabled, Some(bridge.mount())) else {
        return;
    };
    assert!(
        bridged.is_err(),
        "the proxy bridge was accepted for a networked runner"
    );
}
