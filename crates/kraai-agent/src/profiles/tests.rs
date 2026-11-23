use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("kraai-profiles-{name}-{nanos}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn commands() -> HashSet<String> {
    available_command_ids()
}

#[test]
fn built_ins_match_the_locked_command_and_capability_sets() {
    let workspace = temp_dir("built-ins");
    let resolved = resolve_profiles(&workspace, &workspace.join("global"), &commands());
    assert_eq!(
        resolved
            .profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>(),
        vec!["coding", "coding-no-sandbox"]
    );
    let coding = resolved
        .profiles
        .iter()
        .find(|profile| profile.id == "coding")
        .unwrap();
    assert_eq!(coding.commands.len(), 6);
    assert!(
        coding
            .permissions
            .capabilities()
            .contains(SandboxCapability::WorkspaceWrite)
    );
    assert_eq!(coding.environment, EnvironmentPolicy::Inherit);
    assert_eq!(
        coding.nushell_startup,
        if cfg!(windows) {
            NushellStartup::Clean
        } else {
            NushellStartup::Inherit
        }
    );
    assert_eq!(
        coding
            .permissions
            .capabilities()
            .contains(SandboxCapability::HostRead),
        !cfg!(windows)
    );
    assert!(
        !coding
            .permissions
            .capabilities()
            .contains(SandboxCapability::HostWrite)
    );
    assert!(
        !coding
            .permissions
            .capabilities()
            .contains(SandboxCapability::Network)
    );
    assert!(!coding.permissions.capabilities().is_unsandboxed());
    let coding_no_sandbox = resolved
        .profiles
        .iter()
        .find(|profile| profile.id == "coding-no-sandbox")
        .unwrap();
    assert!(
        coding_no_sandbox
            .permissions
            .capabilities()
            .is_unsandboxed()
    );
    assert_eq!(coding_no_sandbox.nushell_startup, coding.nushell_startup);
    let mut equivalent = coding_no_sandbox.clone();
    equivalent.id.clone_from(&coding.id);
    equivalent.display_name.clone_from(&coding.display_name);
    equivalent.description.clone_from(&coding.description);
    equivalent.permissions.clone_from(&coding.permissions);
    equivalent.nushell_startup = coding.nushell_startup;
    assert_eq!(&equivalent, coding);
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn workspace_profiles_replace_built_ins_without_legacy_fields() {
    let workspace = temp_dir("workspace-layer");
    let config_dir = workspace.join(".kraai");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("agents.toml"),
        r#"[[profiles]]
id = "coding"
display_name = "Custom Coding"
description = "custom"
system_prompt = "custom prompt"
commands = ["kraai-open-files"]
capabilities = ["host-read"]
escalation_policy = "deny"
permission_rules = { network = "allow" }
environment = "minimal"
nushell_startup = "clean"
path = "packaged"
"#,
    )
    .unwrap();
    let resolved = resolve_profiles(&workspace, &workspace.join("global"), &commands());
    assert!(resolved.warnings.is_empty());
    let coding = resolved
        .profiles
        .iter()
        .find(|profile| profile.id == "coding")
        .unwrap();
    assert_eq!(coding.display_name, "Custom Coding");
    assert_eq!(coding.escalation_policy, EscalationPolicy::Deny);
    assert_eq!(coding.environment, EnvironmentPolicy::Minimal);
    assert!(
        coding
            .permissions
            .capabilities()
            .contains(SandboxCapability::HostRead)
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn workspace_profiles_extend_built_ins_with_partial_overrides() {
    let workspace = temp_dir("workspace-extends");
    let config_dir = workspace.join(".kraai");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("agents.toml"),
        r#"[[profiles]]
id = "eval-coding"
extends = "coding"
capabilities = ["host-read", "workspace-write"]
escalation_policy = "allow"
environment = "inherit"
path = "inherit"
"#,
    )
    .unwrap();

    let resolved = resolve_profiles(&workspace, &workspace.join("global"), &commands());
    assert!(resolved.warnings.is_empty());
    let coding = resolved
        .profiles
        .iter()
        .find(|profile| profile.id == "coding")
        .unwrap();
    let eval = resolved
        .profiles
        .iter()
        .find(|profile| profile.id == "eval-coding")
        .unwrap();
    assert_eq!(eval.display_name, coding.display_name);
    assert_eq!(eval.description, coding.description);
    assert_eq!(eval.system_prompt, coding.system_prompt);
    assert_eq!(eval.commands, coding.commands);
    assert_eq!(eval.environment, EnvironmentPolicy::Inherit);
    assert_eq!(eval.escalation_policy, EscalationPolicy::Allow);
    assert_eq!(eval.nushell_startup, coding.nushell_startup);
    assert_eq!(eval.path, PathPolicy::Inherit);
    assert!(
        eval.permissions
            .capabilities()
            .contains(SandboxCapability::HostRead)
    );
    assert!(
        eval.permissions
            .capabilities()
            .contains(SandboxCapability::WorkspaceWrite)
    );
    assert_eq!(eval.source, AgentProfileSource::Workspace);
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn extending_an_unknown_profile_rejects_the_layer() {
    let workspace = temp_dir("unknown-extends");
    let config_dir = workspace.join(".kraai");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("agents.toml"),
        r#"[[profiles]]
id = "custom"
extends = "missing"
environment = "inherit"
"#,
    )
    .unwrap();

    let resolved = resolve_profiles(&workspace, &workspace.join("global"), &commands());
    assert_eq!(resolved.warnings.len(), 1);
    assert!(
        resolved
            .warnings
            .first()
            .is_some_and(|warning| warning.message.contains("unknown profile 'missing'"))
    );
    assert!(
        resolved
            .profiles
            .iter()
            .all(|profile| profile.id != "custom")
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn unknown_commands_fail_the_entire_profile_layer() {
    let workspace = temp_dir("unknown-command");
    let config_dir = workspace.join(".kraai");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("agents.toml"),
        r#"[[profiles]]
id = "custom"
display_name = "Custom"
description = "custom"
system_prompt = ""
commands = ["not-installed"]
capabilities = ["workspace-read"]
escalation_policy = "prompt"
environment = "allow-list"
nushell_startup = "clean"
path = "inherit"
"#,
    )
    .unwrap();
    let resolved = resolve_profiles(&workspace, &workspace.join("global"), &commands());
    assert_eq!(resolved.warnings.len(), 1);
    assert!(
        resolved
            .warnings
            .first()
            .is_some_and(|warning| warning.message.contains("unknown command"))
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn inherited_nushell_startup_requires_host_visibility() {
    let workspace = temp_dir("inherited-startup");
    let config_dir = workspace.join(".kraai");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("agents.toml"),
        r#"[[profiles]]
id = "custom"
display_name = "Custom"
description = "custom"
system_prompt = ""
commands = []
capabilities = ["workspace-read"]
escalation_policy = "prompt"
environment = "inherit"
nushell_startup = "inherit"
path = "inherit"
"#,
    )
    .unwrap();

    let resolved = resolve_profiles(&workspace, &workspace.join("global"), &commands());
    assert_eq!(resolved.warnings.len(), 1);
    assert!(
        resolved
            .warnings
            .first()
            .is_some_and(|warning| warning.message.contains("without host-read or no-sandbox"))
    );
    assert!(
        resolved
            .profiles
            .iter()
            .all(|profile| profile.id != "custom")
    );
    let _ = fs::remove_dir_all(workspace);
}
