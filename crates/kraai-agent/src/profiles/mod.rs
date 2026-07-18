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
    display_name: String,
    description: String,
    system_prompt: String,
    #[serde(default)]
    commands: Vec<String>,
    capabilities: Vec<SandboxCapability>,
    escalation_policy: EscalationPolicy,
    #[serde(default)]
    permission_rules: BTreeMap<SandboxCapability, EscalationPolicy>,
    environment: EnvironmentPolicy,
    nushell_startup: NushellStartup,
    path: PathPolicy,
}

pub fn resolve_profiles(
    workspace_dir: &Path,
    available_commands: &HashSet<String>,
) -> ResolvedProfiles {
    let mut resolved = ResolvedProfiles {
        profiles: built_in_profiles(),
        warnings: Vec::new(),
    };

    if let Some(path) = global_profiles_path()
        && let Err(warning) = load_layer(&path, AgentProfileSource::Global, available_commands)
            .map(|profiles| upsert_profiles(&mut resolved.profiles, profiles))
    {
        resolved.warnings.push(warning);
    }

    let workspace_path = workspace_profiles_path(workspace_dir);
    if let Err(warning) = load_layer(
        &workspace_path,
        AgentProfileSource::Workspace,
        available_commands,
    )
    .map(|profiles| upsert_profiles(&mut resolved.profiles, profiles))
    {
        resolved.warnings.push(warning);
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
    for profile in parsed.profiles {
        if !valid_profile_id(&profile.id) {
            return Err(profile_warning(
                source,
                path,
                format!("Invalid profile id '{}'", profile.id),
            ));
        }
        if !seen_ids.insert(profile.id.clone()) {
            return Err(profile_warning(
                source,
                path,
                format!("Duplicate profile id '{}'", profile.id),
            ));
        }

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

        let permissions = SandboxPermissionSet::new(profile.capabilities).map_err(|error| {
            profile_warning(
                source,
                path,
                format!("Profile '{}' has invalid capabilities: {error}", profile.id),
            )
        })?;
        if profile.nushell_startup == NushellStartup::Inherit
            && !permissions
                .capabilities()
                .contains(SandboxCapability::HostRead)
            && !permissions
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
        profiles.push(AgentProfile {
            id: profile.id,
            display_name: profile.display_name,
            description: profile.description,
            system_prompt: profile.system_prompt.trim().to_string(),
            commands: profile.commands,
            permissions,
            permission_rules: CapabilityPermissionRules::new(profile.permission_rules),
            escalation_policy: profile.escalation_policy,
            environment: profile.environment,
            nushell_startup: profile.nushell_startup,
            path: profile.path,
            source,
        });
    }
    Ok(profiles)
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
