use std::path::Path;

use color_eyre::eyre::Result;
use kraai_agent::AgentManager;
use kraai_persistence::Persistence;

use super::provider;

pub(crate) const PROFILE: &str = "performance-offline";
pub(crate) const SOURCES: &[&[u8]] = &[include_bytes!("agent.rs"), include_bytes!("provider.rs")];

pub(crate) fn prepare(directory: &Path) -> Result<()> {
    let workspace = directory.join("workspace");
    std::fs::create_dir_all(workspace.join(".kraai"))?;
    std::fs::write(
        workspace.join(".kraai/agents.toml"),
        r#"[[profiles]]
id = "performance-offline"
display_name = "Offline performance workload"
description = "Fixed local performance workload"
system_prompt = "Return the scripted fixture."
commands = []
capabilities = ["workspace-read"]
escalation_policy = "deny"
environment = "minimal"
nushell_startup = "clean"
path = "inherit"
"#,
    )?;
    Ok(())
}

pub(crate) async fn create(directory: &Path) -> Result<(AgentManager, Persistence)> {
    let workspace = directory.join("workspace");
    let storage = directory.join("storage");
    let persistence = Persistence::open(&storage).await?;
    let manager = AgentManager::new(provider::manager(), workspace, persistence.clone(), storage);
    Ok((manager, persistence))
}
