use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use kraai_execution_types::{
    CapabilityPermissionRules, EnvironmentPolicy, EscalationPolicy, NushellStartup, PathPolicy,
    SandboxCapability, SandboxPermissionSet, ScriptProfileSnapshot,
};
use kraai_types::{AgentProfileSource, AgentProfileSummary, AgentProfileWarning};
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
    pub fn into_summary(self) -> AgentProfileSummary {
        AgentProfileSummary {
            id: self.id,
            display_name: self.display_name,
            description: self.description,
            commands: self.commands,
            capabilities: self.permissions.into_capabilities(),
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
    storage_root: &Path,
    available_commands: &HashSet<String>,
) -> ResolvedProfiles {
    let mut resolved = ResolvedProfiles {
        profiles: built_in_profiles(),
        warnings: Vec::new(),
    };

    {
        let path = std::env::var_os("KRAAI_AGENT_PROFILES")
            .map(PathBuf::from)
            .unwrap_or_else(|| storage_root.join("agents.toml"));
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
    kraai_commands::command_id_set()
}

fn built_in_profiles() -> Vec<AgentProfile> {
    let (read_capability, nushell_startup) = if cfg!(windows) {
        (SandboxCapability::WorkspaceRead, NushellStartup::Clean)
    } else {
        (SandboxCapability::HostRead, NushellStartup::Inherit)
    };

    let coding = AgentProfile {
        id: String::from("coding"),
        display_name: String::from("Coding"),
        description: String::from("Implementation agent with workspace write access"),
        system_prompt: String::new(),
        commands: vec![
            String::from("kraai-open-files"),
            String::from("kraai-close-files"),
            String::from("kraai-web-search"),
            String::from("kraai-mcp"),
            String::from("kraai-view-image"),
            String::from("kraai-edit-file"),
        ],
        permissions: SandboxPermissionSet::new([
            read_capability,
            SandboxCapability::WorkspaceWrite,
        ]),
        permission_rules: CapabilityPermissionRules::default(),
        escalation_policy: EscalationPolicy::Prompt,
        environment: EnvironmentPolicy::Inherit,
        nushell_startup,
        path: PathPolicy::Packaged,
        source: AgentProfileSource::BuiltIn,
    };
    let coding_no_sandbox = AgentProfile {
        id: String::from("coding-no-sandbox"),
        display_name: String::from("Coding (no sandbox)"),
        description: String::from("Implementation agent without sandbox restrictions"),
        permissions: SandboxPermissionSet::new([SandboxCapability::NoSandbox]),
        ..coding.clone()
    };
    vec![coding, coding_no_sandbox]
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
    let Some(contents) =
        kraai_io::fs::read_optional_text(path).map_err(|error| AgentProfileWarning {
            source,
            path: Some(path.display().to_string()),
            message: format!("Failed reading profile file: {error}"),
        })?
    else {
        return Ok(Vec::new());
    };
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
    let permissions = external
        .capabilities
        .map(SandboxPermissionSet::new)
        .or_else(|| base.as_ref().map(|profile| profile.permissions.clone()))
        .ok_or_else(|| field("capabilities"))?;
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
mod tests;
