use super::*;

impl AgentManager {
    pub fn clear_active_turn(&mut self, session_id: &str) {
        if let Some(state) = self.session_states.get_mut(session_id) {
            state.active_turn_profile = None;
        }
    }

    pub fn is_turn_active(&self, session_id: &str) -> bool {
        self.session_states
            .get(session_id)
            .is_some_and(|state| state.active_turn_profile.is_some())
    }

    pub async fn streaming_session_ids(&self) -> HashSet<String> {
        self.streaming_messages
            .read()
            .await
            .values()
            .map(|state| state.session_id.clone())
            .collect()
    }

    pub fn cloned_provider_manager(&self) -> ProviderManager {
        self.providers.clone()
    }

    pub fn new(
        providers: ProviderManager,
        default_workspace_dir: PathBuf,
        message_store: Arc<dyn MessageStore>,
        session_store: Arc<dyn SessionStore>,
        execution_store: Arc<dyn ScriptExecutionStore>,
    ) -> Self {
        let conversation_store =
            ConversationStore::new(message_store.clone(), session_store.clone());
        Self {
            providers,
            default_workspace_dir,
            conversation_store,
            message_store,
            session_store,
            execution_store,
            session_states: HashMap::new(),
            last_used_profile_id: None,
            streaming_messages: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create_session(&mut self) -> Result<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs();

        let session_id = Ulid::new().to_string();
        let session = SessionMeta {
            id: session_id.clone(),
            tip_id: None,
            workspace_dir: self.default_workspace_dir.clone(),
            created_at: now,
            updated_at: now,
            title: None,
            selected_profile_id: Some(
                self.last_used_profile_id
                    .clone()
                    .unwrap_or_else(|| DEFAULT_AGENT_PROFILE_ID.to_string()),
            ),
        };

        self.session_store.save(&session).await?;
        self.ensure_runtime_state(&session_id, &session.workspace_dir);
        Ok(session_id)
    }

    pub async fn prepare_session(&mut self, session_id: &str) -> Result<bool> {
        match self.session_store.get(session_id).await? {
            Some(session) => {
                let session = self.recover_interrupted_stream(session).await?;
                self.ensure_runtime_state(session_id, &session.workspace_dir);
                self.cleanup_hot_cache_for_session(&session).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub async fn prepare_script_recovery(
        &mut self,
        session_id: &str,
        source_message_id: &MessageId,
    ) -> Result<()> {
        if !self.prepare_session(session_id).await? {
            return Err(eyre!("Session not found: {session_id}"));
        }
        let source = self
            .message_store
            .get(source_message_id)
            .await?
            .ok_or_else(|| eyre!("Script source message not found: {source_message_id}"))?;
        let generation = source.generation.ok_or_else(|| {
            eyre!("Script source message {source_message_id} has no provider generation metadata")
        })?;
        let session = self.require_session(session_id).await?;
        let profile = self.resolve_selected_profile(&session)?;
        let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
        state.last_model = Some(generation.model_id);
        state.last_provider = Some(generation.provider_id);
        state.active_turn_profile = Some(profile);
        Ok(())
    }

    /// Roll back an assistant placeholder left by a process that stopped during a stream.
    ///
    /// Provider streams are not resumable. Keeping an incomplete assistant message in the
    /// conversation would make the next request include a response the provider never finished,
    /// so recovery restores the last durable parent message instead.
    pub(super) async fn recover_interrupted_stream(
        &self,
        mut session: SessionMeta,
    ) -> Result<SessionMeta> {
        // A loaded background session can legitimately have a streaming tip. Only recover a
        // placeholder when this process has no corresponding active stream.
        if self.session_has_active_stream(&session.id).await {
            return Ok(session);
        }
        let Some(tip_id) = session.tip_id.clone() else {
            return Ok(session);
        };
        let Some(tip) = self.message_store.get(&tip_id).await? else {
            return Ok(session);
        };
        if !matches!(tip.status, MessageStatus::Streaming { .. }) {
            return Ok(session);
        }

        self.conversation_store
            .restore_tip_title_and_delete_message(
                &session.id,
                &tip_id,
                tip.parent_id.clone(),
                session.title.clone(),
            )
            .await?;
        session.tip_id = tip.parent_id.clone();
        Ok(session)
    }

    pub(super) async fn cleanup_hot_cache_for_session(&self, session: &SessionMeta) -> Result<()> {
        let mut keep_ids = HashSet::new();

        if let Some(tip_id) = &session.tip_id {
            let mut current = Some(tip_id.clone());
            while let Some(id) = current {
                keep_ids.insert(id.clone());
                if let Some(msg) = self.message_store.get(&id).await? {
                    current = msg.parent_id;
                } else {
                    break;
                }
            }
        }

        let hot_ids = self.message_store.list_hot().await?;
        for id in hot_ids.difference(&keep_ids) {
            self.message_store.unload(id).await;
        }

        Ok(())
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionMeta>> {
        self.session_store.list().await
    }

    pub async fn list_user_input_history(&self, limit: usize) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut history = Vec::new();
        for session in self.session_store.list().await? {
            let Some(tip_id) = session.tip_id else {
                continue;
            };

            for message in self.get_history_context(&tip_id).await?.into_iter().rev() {
                if history.len() >= limit {
                    return Ok(history);
                }
                if message.role != ChatRole::User {
                    continue;
                }

                let content = message.content.trim().to_string();
                if !content.is_empty() {
                    history.push(content);
                }
            }
        }

        Ok(history)
    }

    pub async fn delete_session(&mut self, session_id: &str) -> Result<()> {
        self.abort_streaming_messages_for_session(session_id)
            .await?;
        self.session_states.remove(session_id);
        self.session_store.delete(session_id).await
    }

    pub async fn set_workspace_dir(
        &mut self,
        session_id: &str,
        workspace_dir: PathBuf,
    ) -> Result<()> {
        let mut session = self.require_session(session_id).await?;
        session.workspace_dir = workspace_dir.clone();
        session.updated_at = current_unix_timestamp();
        self.session_store.save(&session).await?;

        self.ensure_runtime_state(session_id, &session.workspace_dir)
            .pending_workspace_dir = Some(workspace_dir);
        Ok(())
    }

    pub async fn list_agent_profiles(&mut self, session_id: &str) -> Result<AgentProfilesState> {
        let session = self.require_session(session_id).await?;
        let resolved = self.resolve_profiles_for_workspace(&session.workspace_dir);
        let profile_locked = self.is_profile_locked(session_id);
        Ok(AgentProfilesState {
            profiles: resolved
                .profiles
                .iter()
                .map(AgentProfile::summary)
                .collect(),
            warnings: resolved.warnings,
            selected_profile_id: session.selected_profile_id,
            profile_locked,
        })
    }

    pub async fn set_session_profile(
        &mut self,
        session_id: &str,
        profile_id: String,
    ) -> Result<()> {
        if self.is_profile_locked(session_id) {
            return Err(eyre!(
                "Cannot change profile while the current turn is active"
            ));
        }

        let mut session = self.require_session(session_id).await?;
        let resolved = self.resolve_profiles_for_workspace(&session.workspace_dir);
        let exists = resolved
            .profiles
            .iter()
            .any(|profile| profile.id == profile_id);
        if !exists {
            return Err(eyre!("Unknown profile: {profile_id}"));
        }

        session.selected_profile_id = Some(profile_id);
        session.updated_at = current_unix_timestamp();
        self.session_store.save(&session).await?;
        Ok(())
    }

    pub async fn get_workspace_dir_state(
        &mut self,
        session_id: &str,
    ) -> Result<Option<(PathBuf, bool)>> {
        let Some(session) = self.session_store.get(session_id).await? else {
            return Ok(None);
        };

        let state = self.ensure_runtime_state(session_id, &session.workspace_dir);
        Ok(Some((
            state.effective_workspace_dir(),
            state.pending_workspace_dir.is_some(),
        )))
    }

    pub fn is_profile_locked(&self, session_id: &str) -> bool {
        self.session_states
            .get(session_id)
            .is_some_and(|state| state.active_turn_profile.is_some())
    }

    pub async fn set_providers(
        &mut self,
        config: ProviderManagerConfig,
        registry: ProviderRegistry,
    ) -> Result<()> {
        self.providers.load_config(config, registry).await
    }

    pub async fn list_models(&self) -> HashMap<ProviderId, Vec<Model>> {
        self.providers.list_all_models().await
    }

    pub async fn get_tip(&self, session_id: &str) -> Result<Option<MessageId>> {
        Ok(self
            .session_store
            .get(session_id)
            .await?
            .and_then(|session| session.tip_id))
    }

    pub(super) async fn set_tip(&self, session_id: &str, new_tip: Option<MessageId>) -> Result<()> {
        if let Some(mut session) = self.session_store.get(session_id).await? {
            session.tip_id = new_tip;
            session.updated_at = current_unix_timestamp();
            self.session_store.save(&session).await?;
        }

        Ok(())
    }

    pub(super) fn ensure_runtime_state(
        &mut self,
        session_id: &str,
        workspace_dir: &Path,
    ) -> &mut SessionRuntimeState {
        self.session_states
            .entry(session_id.to_string())
            .or_insert_with(|| SessionRuntimeState::new(workspace_dir.to_path_buf()))
    }

    pub(super) fn resolve_profiles_for_workspace(&self, workspace_dir: &Path) -> ResolvedProfiles {
        resolve_profiles(workspace_dir, &crate::profiles::available_command_ids())
    }

    pub(super) fn resolve_selected_profile(&self, session: &SessionMeta) -> Result<AgentProfile> {
        let Some(profile_id) = session.selected_profile_id.as_ref() else {
            return Err(eyre!("No profile selected for this session"));
        };

        self.resolve_profiles_for_workspace(&session.workspace_dir)
            .profiles
            .into_iter()
            .find(|profile| &profile.id == profile_id)
            .ok_or_else(|| eyre!("Selected profile is unavailable: {profile_id}"))
    }

    pub(super) async fn require_session(&self, session_id: &str) -> Result<SessionMeta> {
        self.session_store
            .get(session_id)
            .await?
            .ok_or_else(|| eyre!("Session not found: {session_id}"))
    }

    pub fn script_turn_context(&self, session_id: &str) -> Result<ScriptTurnContext> {
        let state = self
            .session_states
            .get(session_id)
            .ok_or_else(|| eyre!("Session runtime state is unavailable: {session_id}"))?;
        let profile = state
            .active_turn_profile
            .as_ref()
            .ok_or_else(|| eyre!("Session has no active turn: {session_id}"))?;
        Ok(ScriptTurnContext {
            workspace_dir: state.active_workspace_dir.clone(),
            profile: profile.snapshot(),
        })
    }
}
