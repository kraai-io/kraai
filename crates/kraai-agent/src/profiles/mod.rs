use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use kraai_persistence::agent_state_root;
use kraai_types::{
    AgentProfileSource, AgentProfileSummary, AgentProfileWarning, CapabilityPermissionRules,
    EnvironmentPolicy, EscalationPolicy, NushellStartup, PathPolicy, SandboxCapability,
    SandboxPermissionSet, ScriptProfileSnapshot,
};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfile {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub system_prompt: String,
    pub commands: Vec<String>,
    pub permissions: SandboxPermissionSet,
    pub permission_rules: CapabilityPermissionRules,
    pub escalation_policy: EscalationPolicy,
    pub environment: EnvironmentPolicy,
    pub nushell_startup: NushellStartup,
    pub path: PathPolicy,
    pub source: AgentProfileSource,
}

impl AgentProfile {
    pub fn summary(&self) -> AgentProfileSummary {
        AgentProfileSummary {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            description: self.description.clone(),
            commands: self.commands.clone(),
            capabilities: self.permissions.capabilities().clone(),
            escalation_policy: self.escalation_policy,
            environment: self.environment,
            nushell_startup: self.nushell_startup,
            path: self.path,
            source: self.source,
        }
    }

    pub fn snapshot(&self) -> ScriptProfileSnapshot {
        ScriptProfileSnapshot {
            id: self.id.clone(),
            commands: self.commands.clone(),
            permissions: self.permissions.clone(),
            permission_rules: self.permission_rules.clone(),
            escalation_policy: self.escalation_policy,
            environment: self.environment,
            nushell_startup: self.nushell_startup,
            path: self.path,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedProfiles {
    pub profiles: Vec<AgentProfile>,
    pub warnings: Vec<AgentProfileWarning>,
}

#[derive(Debug, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    profiles: Vec<ExternalProfile>,
}

#[derive(Debug, Deserialize)]
struct ExternalProfile {
    id: String,
    extends: Option<String>,
    display_name: Option<String>,
    description: Option<String>,
    system_prompt: Option<String>,
    commands: Option<Vec<String>>,
    capabilities: Option<Vec<SandboxCapability>>,
    escalation_policy: Option<EscalationPolicy>,
    permission_rules: Option<BTreeMap<SandboxCapability, EscalationPolicy>>,
    environment: Option<EnvironmentPolicy>,
    nushell_startup: Option<NushellStartup>,
    path: Option<PathPolicy>,
}

pub fn resolve_profiles(
    workspace_dir: &Path,
    available_commands: &HashSet<String>,
) -> ResolvedProfiles {
    let mut resolved = ResolvedProfiles {
        profiles: built_in_profiles(),
        warnings: Vec::new(),
    };

    if let Some(path) = global_profiles_path() {
        match load_layer(
            &path,
            AgentProfileSource::Global,
            available_commands,
            &resolved.profiles,
        ) {
            Ok(profiles) => upsert_profiles(&mut resolved.profiles, profiles),
            Err(warning) => resolved.warnings.push(warning),
        }
    }

    let workspace_path = workspace_profiles_path(workspace_dir);
    match load_layer(
        &workspace_path,
        AgentProfileSource::Workspace,
        available_commands,
        &resolved.profiles,
    ) {
        Ok(profiles) => upsert_profiles(&mut resolved.profiles, profiles),
        Err(warning) => resolved.warnings.push(warning),
    }

    resolved
}

pub fn available_command_ids() -> HashSet<String> {
    ["kraai-open-files", "kraai-close-files", "kraai-edit-file"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn built_in_profiles() -> Vec<AgentProfile> {
    let common = || {
        (
            CapabilityPermissionRules::default(),
            EscalationPolicy::Prompt,
            EnvironmentPolicy::AllowList,
            NushellStartup::Clean,
            PathPolicy::Packaged,
        )
    };
    let (plan_rules, plan_escalation, plan_environment, plan_startup, plan_path) = common();
    let (coding_rules, coding_escalation, coding_environment, coding_startup, coding_path) =
        common();
    vec![
        AgentProfile {
            id: String::from("plan"),
            display_name: String::from("Plan"),
            description: String::from("Read-only planning and investigation agent"),
            system_prompt: include_str!("plan_code.md").trim().to_string(),
            commands: vec![
                String::from("kraai-open-files"),
                String::from("kraai-close-files"),
            ],
            permissions: SandboxPermissionSet::workspace_read(),
            permission_rules: plan_rules,
            escalation_policy: plan_escalation,
            environment: plan_environment,
            nushell_startup: plan_startup,
            path: plan_path,
            source: AgentProfileSource::BuiltIn,
        },
        AgentProfile {
            id: String::from("coding"),
            display_name: String::from("Coding"),
            description: String::from("Implementation agent with workspace write access"),
            system_prompt: include_str!("build_code.md").trim().to_string(),
            commands: vec![
                String::from("kraai-open-files"),
                String::from("kraai-close-files"),
                String::from("kraai-edit-file"),
            ],
            permissions: SandboxPermissionSet::workspace_write(),
            permission_rules: coding_rules,
            escalation_policy: coding_escalation,
            environment: coding_environment,
            nushell_startup: coding_startup,
            path: coding_path,
            source: AgentProfileSource::BuiltIn,
        },
    ]
}

fn global_profiles_path() -> Option<PathBuf> {
    agent_state_root().ok().map(|path| path.join("agents.toml"))
}

fn workspace_profiles_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".kraai/agents.toml")
}

fn load_layer(
    path: &Path,
    source: AgentProfileSource,
    available_commands: &HashSet<String>,
    inherited_profiles: &[AgentProfile],
) -> Result<Vec<AgentProfile>, AgentProfileWarning> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let contents = std::fs::read_to_string(path).map_err(|error| AgentProfileWarning {
        source,
        path: Some(path.display().to_string()),
        message: format!("Failed reading profile file: {error}"),
    })?;
    let parsed: ProfilesFile = toml::from_str(&contents).map_err(|error| AgentProfileWarning {
        source,
        path: Some(path.display().to_string()),
        message: format!("Failed parsing profile file: {error}"),
    })?;

    let mut seen_ids = HashSet::new();
    let mut profiles = Vec::with_capacity(parsed.profiles.len());
    for external in parsed.profiles {
        if !valid_profile_id(&external.id) {
            return Err(profile_warning(
                source,
                path,
                format!("Invalid profile id '{}'", external.id),
            ));
        }
        if !seen_ids.insert(external.id.clone()) {
            return Err(profile_warning(
                source,
                path,
                format!("Duplicate profile id '{}'", external.id),
            ));
        }

        let profile = resolve_external_profile(external, inherited_profiles, source, path)?;

        let mut seen_commands = HashSet::new();
        for command in &profile.commands {
            if !seen_commands.insert(command) {
                return Err(profile_warning(
                    source,
                    path,
                    format!(
                        "Profile '{}' selects command '{}' more than once",
                        profile.id, command
                    ),
                ));
            }
            if !available_commands.contains(command) {
                return Err(profile_warning(
                    source,
                    path,
                    format!(
                        "Profile '{}' references unknown command '{}'",
                        profile.id, command
                    ),
                ));
            }
        }

        if profile.nushell_startup == NushellStartup::Inherit
            && !profile
                .permissions
                .capabilities()
                .contains(SandboxCapability::HostRead)
            && !profile
                .permissions
                .capabilities()
                .contains(SandboxCapability::NoSandbox)
        {
            return Err(profile_warning(
                source,
                path,
                format!(
                    "Profile '{}' uses inherited Nushell startup files without host-read or no-sandbox",
                    profile.id
                ),
            ));
        }
        profiles.push(profile);
    }
    Ok(profiles)
}

fn resolve_external_profile(
    external: ExternalProfile,
    inherited_profiles: &[AgentProfile],
    source: AgentProfileSource,
    path: &Path,
) -> Result<AgentProfile, AgentProfileWarning> {
    let base = external
        .extends
        .as_ref()
        .map(|base_id| {
            inherited_profiles
                .iter()
                .find(|profile| profile.id == *base_id)
                .cloned()
                .ok_or_else(|| {
                    profile_warning(
                        source,
                        path,
                        format!(
                            "Profile '{}' extends unknown profile '{base_id}'",
                            external.id
                        ),
                    )
                })
        })
        .transpose()?;
    let field = |name: &str| {
        profile_warning(
            source,
            path,
            format!(
                "Profile '{}' must define '{name}' or extend another profile",
                external.id
            ),
        )
    };

    let display_name = external
        .display_name
        .or_else(|| base.as_ref().map(|profile| profile.display_name.clone()))
        .ok_or_else(|| field("display_name"))?;
    let description = external
        .description
        .or_else(|| base.as_ref().map(|profile| profile.description.clone()))
        .ok_or_else(|| field("description"))?;
    let system_prompt = external
        .system_prompt
        .or_else(|| base.as_ref().map(|profile| profile.system_prompt.clone()))
        .ok_or_else(|| field("system_prompt"))?;
    let commands = external
        .commands
        .or_else(|| base.as_ref().map(|profile| profile.commands.clone()))
        .unwrap_or_default();
    let permissions = external.capabilities.map_or_else(
        || {
            base.as_ref()
                .map(|profile| profile.permissions.clone())
                .ok_or_else(|| field("capabilities"))
        },
        |capabilities| {
            SandboxPermissionSet::new(capabilities).map_err(|error| {
                profile_warning(
                    source,
                    path,
                    format!(
                        "Profile '{}' has invalid capabilities: {error}",
                        external.id
                    ),
                )
            })
        },
    )?;
    let permission_rules = external.permission_rules.map_or_else(
        || {
            base.as_ref()
                .map_or_else(CapabilityPermissionRules::default, |profile| {
                    profile.permission_rules.clone()
                })
        },
        CapabilityPermissionRules::new,
    );
    let escalation_policy = external
        .escalation_policy
        .or_else(|| base.as_ref().map(|profile| profile.escalation_policy))
        .ok_or_else(|| field("escalation_policy"))?;
    let environment = external
        .environment
        .or_else(|| base.as_ref().map(|profile| profile.environment))
        .ok_or_else(|| field("environment"))?;
    let nushell_startup = external
        .nushell_startup
        .or_else(|| base.as_ref().map(|profile| profile.nushell_startup))
        .ok_or_else(|| field("nushell_startup"))?;
    let path_policy = external
        .path
        .or_else(|| base.as_ref().map(|profile| profile.path))
        .ok_or_else(|| field("path"))?;

    Ok(AgentProfile {
        id: external.id,
        display_name,
        description,
        system_prompt: system_prompt.trim().to_string(),
        commands,
        permissions,
        permission_rules,
        escalation_policy,
        environment,
        nushell_startup,
        path: path_policy,
        source,
    })
}

fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn profile_warning(
    source: AgentProfileSource,
    path: &Path,
    message: String,
) -> AgentProfileWarning {
    AgentProfileWarning {
        source,
        path: Some(path.display().to_string()),
        message,
    }
}

fn upsert_profiles(existing: &mut Vec<AgentProfile>, layer: Vec<AgentProfile>) {
    for profile in layer {
        if let Some(current) = existing.iter_mut().find(|current| current.id == profile.id) {
            *current = profile;
        } else {
            existing.push(profile);
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "profile tests use direct assertions for temporary configuration fixtures"
)]
mod tests {
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
        let resolved = resolve_profiles(&workspace, &commands());
        let plan = resolved
            .profiles
            .iter()
            .find(|profile| profile.id == "plan")
            .unwrap();
        let coding = resolved
            .profiles
            .iter()
            .find(|profile| profile.id == "coding")
            .unwrap();
        assert_eq!(plan.commands.len(), 2);
        assert!(
            plan.permissions
                .capabilities()
                .contains(SandboxCapability::WorkspaceRead)
        );
        assert!(
            !plan
                .permissions
                .capabilities()
                .contains(SandboxCapability::WorkspaceWrite)
        );
        assert_eq!(coding.commands.len(), 3);
        assert!(
            coding
                .permissions
                .capabilities()
                .contains(SandboxCapability::WorkspaceWrite)
        );
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
id = "plan"
display_name = "Custom Plan"
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
        let resolved = resolve_profiles(&workspace, &commands());
        assert!(resolved.warnings.is_empty());
        let plan = resolved
            .profiles
            .iter()
            .find(|profile| profile.id == "plan")
            .unwrap();
        assert_eq!(plan.display_name, "Custom Plan");
        assert_eq!(plan.escalation_policy, EscalationPolicy::Deny);
        assert_eq!(plan.environment, EnvironmentPolicy::Minimal);
        assert!(
            plan.permissions
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

        let resolved = resolve_profiles(&workspace, &commands());
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
        assert_eq!(eval.nushell_startup, NushellStartup::Clean);
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

        let resolved = resolve_profiles(&workspace, &commands());
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
        let resolved = resolve_profiles(&workspace, &commands());
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

        let resolved = resolve_profiles(&workspace, &commands());
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
}
