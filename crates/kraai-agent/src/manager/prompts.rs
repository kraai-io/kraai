use super::*;

impl AgentManager {
    pub(super) fn build_system_prompt(&self, profile: &AgentProfile) -> Result<String> {
        let tool_prompt = self
            .tools
            .generate_system_prompt_for_tools(&profile.tools)
            .map_err(|error| eyre!(error.to_string()))?;
        if profile.system_prompt.is_empty() {
            Ok(tool_prompt)
        } else if tool_prompt.is_empty() {
            Ok(profile.system_prompt.clone())
        } else {
            Ok(format!("{}\n\n{}", profile.system_prompt, tool_prompt))
        }
    }

    pub(super) fn load_workspace_agents_md_prompt(
        &self,
        workspace_dir: &Path,
    ) -> Result<Option<String>> {
        let agents_path = workspace_dir.join(AGENTS_MD_FILE_NAME);
        let contents = match std::fs::read_to_string(&agents_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(eyre!("Failed reading {}: {error}", agents_path.display())),
        };

        if contents.trim().is_empty() {
            return Ok(None);
        }

        Ok(Some(format!(
            "Workspace Instructions\nThe following instructions come from {AGENTS_MD_FILE_NAME} in the active workspace. Follow them in addition to the rest of this system prompt.\n\n```markdown\n{contents}\n```"
        )))
    }

    pub(super) fn build_turn_system_prompt(
        &self,
        session_id: &str,
        profile: &AgentProfile,
        workspace_dir: &Path,
        tool_state_snapshot: &mut ToolStateSnapshot,
    ) -> Result<String> {
        let mut sections = Vec::new();

        let base_system_prompt = self.build_system_prompt(profile)?;
        if !base_system_prompt.is_empty() {
            sections.push(base_system_prompt);
        }

        if let Some(workspace_agents_prompt) =
            self.load_workspace_agents_md_prompt(workspace_dir)?
        {
            sections.push(workspace_agents_prompt);
        }

        let tool_state_prompt = render_tool_state_prompt(tool_state_snapshot, workspace_dir);
        if !tool_state_prompt.is_empty() {
            sections.push(tool_state_prompt);
        }

        let system_prompt = sections.join("\n\n");
        #[cfg(debug_assertions)]
        {
            if system_prompt.is_empty() {
                tracing::info!(
                    session_id = session_id,
                    profile_id = %profile.id,
                    "Compiled turn system prompt is empty"
                );
            } else {
                tracing::info!(
                    session_id = session_id,
                    profile_id = %profile.id,
                    "Compiled turn system prompt:\n{}",
                    system_prompt
                );
            }
        }

        #[cfg(not(debug_assertions))]
        let _ = (session_id, profile, &system_prompt);

        Ok(system_prompt)
    }

    pub(super) async fn resolve_model_max_context(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
    ) -> Option<usize> {
        self.providers
            .get_provider(provider_id)?
            .list_models()
            .await
            .into_iter()
            .find(|model| model.id == *model_id)
            .and_then(|model| model.max_context)
    }
}
