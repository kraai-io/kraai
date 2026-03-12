use std::collections::HashSet;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use serde::Deserialize;
use types::{AgentProfileSource, AgentProfileSummary, AgentProfileWarning, RiskLevel, ToolId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfile {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub system_prompt: String,
    pub tools: Vec<ToolId>,
    pub default_risk_level: RiskLevel,
    pub source: AgentProfileSource,
}

impl AgentProfile {
    pub fn summary(&self) -> AgentProfileSummary {
        AgentProfileSummary {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            description: self.description.clone(),
            tools: self.tools.iter().map(|tool| tool.to_string()).collect(),
            default_risk_level: self.default_risk_level,
            source: self.source,
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
    tools: Vec<String>,
    default_risk_level: String,
}

pub fn resolve_profiles(
    workspace_dir: &Path,
    available_tools: &HashSet<String>,
) -> ResolvedProfiles {
    let mut resolved = ResolvedProfiles {
        profiles: built_in_profiles(),
        warnings: Vec::new(),
    };

    if let Some(path) = global_profiles_path()
        && let Err(warning) = load_layer(&path, AgentProfileSource::Global, available_tools)
            .map(|profiles| upsert_profiles(&mut resolved.profiles, profiles))
    {
        resolved.warnings.push(warning);
    }

    let workspace_path = workspace_profiles_path(workspace_dir);
    if let Err(warning) = load_layer(
        &workspace_path,
        AgentProfileSource::Workspace,
        available_tools,
    )
    .map(|profiles| upsert_profiles(&mut resolved.profiles, profiles))
    {
        resolved.warnings.push(warning);
    }

    resolved
}

fn built_in_profiles() -> Vec<AgentProfile> {
    vec![
        AgentProfile {
            id: String::from("plan-code"),
            display_name: String::from("Plan Code"),
            description: String::from("Read-only planning and investigation agent"),
            system_prompt: include_str!("plan_code.md").trim().to_string(),
            tools: vec![
                ToolId::new("list_files"),
                ToolId::new("search_files"),
                ToolId::new("read_files"),
            ],
            default_risk_level: RiskLevel::ReadOnlyWorkspace,
            source: AgentProfileSource::BuiltIn,
        },
        AgentProfile {
            id: String::from("build-code"),
            display_name: String::from("Build Code"),
            description: String::from("Implementation agent with workspace write access"),
            system_prompt: include_str!("build_code.md").trim().to_string(),
            tools: vec![
                ToolId::new("list_files"),
                ToolId::new("search_files"),
                ToolId::new("read_files"),
                ToolId::new("edit_file"),
            ],
            default_risk_level: RiskLevel::UndoableWorkspaceWrite,
            source: AgentProfileSource::BuiltIn,
        },
    ]
}

fn global_profiles_path() -> Option<PathBuf> {
    let base_dirs = BaseDirs::new()?;
    Some(base_dirs.home_dir().join(".agent-desktop/agents.toml"))
}

fn workspace_profiles_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".agent/agents.toml")
}

fn load_layer(
    path: &Path,
    source: AgentProfileSource,
    available_tools: &HashSet<String>,
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
        if !seen_ids.insert(profile.id.clone()) {
            return Err(AgentProfileWarning {
                source,
                path: Some(path.display().to_string()),
                message: format!("Duplicate profile id '{}'", profile.id),
            });
        }

        if profile.tools.is_empty() {
            return Err(AgentProfileWarning {
                source,
                path: Some(path.display().to_string()),
                message: format!("Profile '{}' must declare at least one tool", profile.id),
            });
        }

        let Some(default_risk_level) = RiskLevel::parse(&profile.default_risk_level) else {
            return Err(AgentProfileWarning {
                source,
                path: Some(path.display().to_string()),
                message: format!(
                    "Profile '{}' has invalid default_risk_level '{}'",
                    profile.id, profile.default_risk_level
                ),
            });
        };

        let mut tools = Vec::with_capacity(profile.tools.len());
        for tool in &profile.tools {
            if !available_tools.contains(tool) {
                return Err(AgentProfileWarning {
                    source,
                    path: Some(path.display().to_string()),
                    message: format!(
                        "Profile '{}' references unknown tool '{}'",
                        profile.id, tool
                    ),
                });
            }
            tools.push(ToolId::new(tool));
        }

        profiles.push(AgentProfile {
            id: profile.id,
            display_name: profile.display_name,
            description: profile.description,
            system_prompt: profile.system_prompt.trim().to_string(),
            tools,
            default_risk_level,
            source,
        });
    }

    Ok(profiles)
}

fn upsert_profiles(existing: &mut Vec<AgentProfile>, layer: Vec<AgentProfile>) {
    for profile in layer {
        if let Some(index) = existing.iter().position(|current| current.id == profile.id) {
            existing[index] = profile;
        } else {
            existing.push(profile);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{AgentProfileSource, load_layer, resolve_profiles, workspace_profiles_path};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("agent-profiles-{name}-{nanos}"))
    }

    fn available_tools() -> HashSet<String> {
        ["list_files", "search_files", "read_files", "edit_file"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn built_in_catalog_contains_plan_and_build() {
        let dir = temp_dir("builtins");
        let resolved = resolve_profiles(&dir, &available_tools());
        let ids = resolved
            .profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"plan-code"));
        assert!(ids.contains(&"build-code"));
    }

    #[test]
    fn workspace_profiles_override_built_ins_by_id() {
        let dir = temp_dir("workspace-override");
        let profile_dir = dir.join(".agent");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            workspace_profiles_path(&dir),
            r#"
[[profiles]]
id = "plan-code"
display_name = "Override"
description = "Override description"
system_prompt = "workspace"
tools = ["list_files"]
default_risk_level = "read_only_workspace"
"#,
        )
        .unwrap();

        let resolved = resolve_profiles(&dir, &available_tools());
        let plan = resolved
            .profiles
            .iter()
            .find(|profile| profile.id == "plan-code")
            .unwrap();
        assert_eq!(plan.display_name, "Override");
        assert_eq!(plan.source, AgentProfileSource::Workspace);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_workspace_layer_is_ignored_with_warning() {
        let dir = temp_dir("workspace-invalid");
        let profile_dir = dir.join(".agent");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            workspace_profiles_path(&dir),
            r#"
[[profiles]]
id = "broken"
display_name = "Broken"
description = "Broken"
system_prompt = "workspace"
tools = ["missing_tool"]
default_risk_level = "read_only_workspace"
"#,
        )
        .unwrap();

        let resolved = resolve_profiles(&dir, &available_tools());
        assert!(
            resolved
                .profiles
                .iter()
                .any(|profile| profile.id == "plan-code")
        );
        assert!(
            resolved
                .profiles
                .iter()
                .any(|profile| profile.id == "build-code")
        );
        assert!(!resolved.warnings.is_empty());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn layer_loader_rejects_invalid_risk_level() {
        let dir = temp_dir("invalid-risk");
        fs::write(
            &dir,
            r#"
[[profiles]]
id = "broken"
display_name = "Broken"
description = "Broken"
system_prompt = "workspace"
tools = ["list_files"]
default_risk_level = "not_real"
"#,
        )
        .unwrap();

        let warning = load_layer(&dir, AgentProfileSource::Global, &available_tools()).unwrap_err();
        assert!(warning.message.contains("invalid default_risk_level"));

        let _ = fs::remove_file(dir);
    }
}
