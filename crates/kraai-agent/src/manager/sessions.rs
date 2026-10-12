use super::*;

impl AgentManager {
    pub fn clear_active_turn(&self, session_id: &SessionId) {
        if let Some(state) = lock(&self.session_states).get_mut(session_id) {
            state.active_turn_profile = None;
        }
    }

    pub fn is_turn_active(&self, session_id: &SessionId) -> bool {
        lock(&self.session_states)
            .get(session_id)
            .is_some_and(|state| state.active_turn_profile.is_some())
    }

    pub async fn streaming_session_ids(&self) -> HashSet<SessionId> {
        self.streaming_messages
            .read()
            .await
            .values()
            .map(|state| state.session_id.clone())
            .collect()
    }

    pub async fn get_message(&self, id: &MessageId) -> Result<Option<Message>> {
        self.message_store.get(id).await
    }

    pub fn shared_session_store(&self) -> Arc<kraai_persistence::SqliteSessionStore> {
        self.persistence.sessions().clone()
    }

    pub fn cloned_provider_manager(&self) -> ProviderManager {
        read(&self.providers).clone()
    }

    pub fn new(
        providers: ProviderManager,
        default_workspace_dir: PathBuf,
        persistence: kraai_persistence::Persistence,
        storage_root: PathBuf,
    ) -> Self {
        let conversation_store = persistence.conversations();
        Self {
            mcp: StdRwLock::default(),
            session_mcp: StdMutex::default(),
            providers: StdRwLock::new(providers),
            default_workspace_dir,
            user_agents_path: Some(storage_root.join(AGENTS_MD_FILE_NAME)),
            storage_root,
            conversation_store,
            message_store: persistence.messages().clone(),
            session_store: persistence.sessions().clone(),
            context_state_store: persistence.context().clone(),
            usage_store: persistence.usage().clone(),
            persistence,
            session_states: StdMutex::default(),
            last_used_profile_id: StdMutex::default(),
            streaming_messages: RwLock::new(BTreeMap::new()),
            streaming_persistence: Mutex::new(()),
        }
    }

    pub async fn create_session(&self) -> Result<SessionId> {
        self.create_session_with(None, None).await
    }

    pub async fn create_session_with(
        &self,
        workspace_dir: Option<PathBuf>,
        profile_id: Option<String>,
    ) -> Result<SessionId> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs();

        let session_id = SessionId::new(Ulid::generate());
        let workspace_dir = workspace_dir.unwrap_or_else(|| self.default_workspace_dir.clone());
        let resolved = self.resolve_profiles_for_workspace(&workspace_dir);
        let selected_profile_id =
            profile_id.unwrap_or_else(|| self.default_profile_for_profiles(&resolved.profiles));
        if !resolved
            .profiles
            .iter()
            .any(|profile| profile.id == selected_profile_id)
        {
            return Err(eyre!(kraai_types::DomainError::not_found(format!(
                "Unknown profile: {selected_profile_id}"
            ))));
        }
        let session = SessionMeta {
            revision: 0,
            id: session_id.clone(),
            tip_id: None,
            workspace_dir: workspace_dir.clone(),
            created_at: now,
            updated_at: now,
            title: None,
            selected_profile_id: Some(selected_profile_id.clone()),
            selected_model: None,
        };

        self.session_store.save(&session).await?;
        self.with_runtime_state(&session_id, &session.workspace_dir, |_| ());
        *lock(&self.last_used_profile_id) = Some(selected_profile_id);
        Ok(session_id)
    }

    pub fn list_agent_profiles_for_workspace(
        &self,
        workspace_dir: Option<&Path>,
    ) -> (PathBuf, AgentProfilesState) {
        let workspace_dir = workspace_dir
            .unwrap_or(&self.default_workspace_dir)
            .to_path_buf();
        let resolved = self.resolve_profiles_for_workspace(&workspace_dir);
        let selected_profile_id = Some(self.default_profile_for_profiles(&resolved.profiles));
        (
            workspace_dir,
            AgentProfilesState {
                profiles: resolved
                    .profiles
                    .into_iter()
                    .map(AgentProfile::into_summary)
                    .collect(),
                warnings: resolved.warnings,
                selected_profile_id,
                profile_locked: false,
            },
        )
    }

    fn default_profile_for_profiles(&self, profiles: &[AgentProfile]) -> String {
        lock(&self.last_used_profile_id)
            .as_ref()
            .filter(|selected| profiles.iter().any(|profile| &profile.id == *selected))
            .cloned()
            .unwrap_or_else(|| DEFAULT_AGENT_PROFILE_ID.to_string())
    }

    pub async fn prepare_session(&self, session_id: &SessionId) -> Result<bool> {
        match self.session_store.get(session_id).await? {
            Some(session) => {
                self.with_runtime_state(session_id, &session.workspace_dir, |_| ());
                self.validate_history_for_session(&session).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub async fn prepare_script_recovery(
        &self,
        session_id: &SessionId,
        source_message_id: &MessageId,
    ) -> Result<()> {
        let source = self
            .message_store
            .get(source_message_id)
            .await?
            .ok_or_else(|| eyre!("Script source message not found: {source_message_id}"))?;
        let generation = source.generation.ok_or_else(|| {
            eyre!("Script source message {source_message_id} has no provider generation metadata")
        })?;
        self.validate_model_options(
            &generation.provider_id,
            &generation.model_id,
            &generation.options,
            true,
        )
        .await?;
        if !self.prepare_session(session_id).await? {
            return Err(eyre!(kraai_types::DomainError::not_found(format!(
                "Session not found: {session_id}"
            ))));
        }
        let session = self.require_session(session_id).await?;
        let profile = self.resolve_selected_profile(&session)?;
        self.with_runtime_state(session_id, &session.workspace_dir, |state| {
            state.last_model = Some(generation.model_id);
            state.last_provider = Some(generation.provider_id);
            state.last_options = generation.options;
            state.active_turn_profile = Some(Arc::new(profile));
        });
        Ok(())
    }

    /// Rolls back a streaming placeholder left by a stopped runtime; requires the session lease.
    pub async fn recover_interrupted_session(&self, session_id: &SessionId) -> Result<()> {
        let session = self.require_session(session_id).await?;
        self.recover_interrupted_stream(session).await?;
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
        if !self.persistence.sessions().owns_turn(&session.id).await? {
            return Ok(session);
        }
        if self.session_has_active_stream(&session.id).await {
            return Ok(session);
        }
        let Some(tip_id) = session.tip_id.clone() else {
            return Ok(session);
        };
        let Some(tip) = self.message_store.get(&tip_id).await? else {
            return Ok(session);
        };
        if tip.status == MessageStatus::Complete
            && let ConversationItem::ScriptResult { call_id, .. } = &tip.content
            && let Some(parent_id) = &tip.parent_id
            && let Some(mut parent) = self.message_store.get(parent_id).await?
            && matches!(parent.status, MessageStatus::Streaming { .. })
            && matches!(&parent.content, ConversationItem::Assistant { items }
                if items.iter().any(|item| matches!(item, AssistantItem::ScriptCall { call_id: parent_call, .. }
                    if parent_call == call_id)))
        {
            parent.status = MessageStatus::Complete;
            self.message_store.save(&parent).await?;
        }
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

    pub(super) async fn validate_history_for_session(&self, session: &SessionMeta) -> Result<()> {
        HistoryCursor::new(self.message_store.as_ref(), session.tip_id.clone())
            .read_to_root()
            .await?;
        Ok(())
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionMeta>> {
        self.session_store.list().await
    }

    pub async fn list_session_ids(&self) -> Result<HashSet<SessionId>> {
        self.session_store.list_ids().await
    }

    pub async fn list_user_input_history(&self, limit: usize) -> Result<Vec<String>> {
        let mut history = Vec::new();
        if limit == 0 {
            return Ok(history);
        }
        let in_flight = self.capture_in_flight_messages(None).await;
        for session in self.session_store.list().await? {
            let mut messages = HistoryCursor::new(self.message_store.as_ref(), session.tip_id)
                .with_overlay(&in_flight);
            while let Some(page) = messages.next_page().await? {
                for message in page {
                    if history.len() >= limit {
                        return Ok(history);
                    }
                    if let ConversationItem::User { content } = &message.content {
                        let content = content.text_only().trim().to_string();
                        if !content.is_empty() {
                            history.push(content);
                        }
                    }
                }
            }
        }
        Ok(history)
    }

    pub async fn delete_session(&self, session_id: &SessionId) -> Result<()> {
        self.persistence
            .sessions()
            .ensure_writable(session_id)
            .await?;
        self.abort_streaming_messages_for_session(session_id)
            .await?;
        let mcp = lock(&self.session_mcp).remove(session_id);
        if let Some(mcp) = mcp {
            mcp.shutdown().await;
        }
        lock(&self.session_states).remove(session_id);
        self.persistence.delete_session(session_id).await
    }

    pub async fn set_workspace_dir(
        &self,
        session_id: &SessionId,
        workspace_dir: PathBuf,
    ) -> Result<()> {
        let mut session = self.require_session(session_id).await?;
        session.workspace_dir = workspace_dir.clone();
        session.updated_at = current_unix_timestamp();
        self.session_store.save(&session).await?;

        self.with_runtime_state(session_id, &session.workspace_dir, |state| {
            state.pending_workspace_dir = Some(workspace_dir);
        });
        Ok(())
    }

    pub async fn list_agent_profiles(&self, session_id: &SessionId) -> Result<AgentProfilesState> {
        let session = self.require_session(session_id).await?;
        let resolved = self.resolve_profiles_for_workspace(&session.workspace_dir);
        let profile_locked = self.is_profile_locked(session_id);
        Ok(AgentProfilesState {
            profiles: resolved
                .profiles
                .into_iter()
                .map(AgentProfile::into_summary)
                .collect(),
            warnings: resolved.warnings,
            selected_profile_id: session.selected_profile_id,
            profile_locked,
        })
    }

    pub async fn set_session_profile(
        &self,
        session_id: &SessionId,
        profile_id: String,
    ) -> Result<()> {
        if self.is_profile_locked(session_id) {
            return Err(eyre!(kraai_types::DomainError::conflict(
                "Cannot change profile while the current turn is active"
            )));
        }

        let mut session = self.require_session(session_id).await?;
        let resolved = self.resolve_profiles_for_workspace(&session.workspace_dir);
        let exists = resolved
            .profiles
            .iter()
            .any(|profile| profile.id == profile_id);
        if !exists {
            return Err(eyre!(kraai_types::DomainError::not_found(format!(
                "Unknown profile: {profile_id}"
            ))));
        }

        session.selected_profile_id = Some(profile_id);
        session.updated_at = current_unix_timestamp();
        self.session_store.save(&session).await?;
        Ok(())
    }

    pub async fn get_session_model(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<kraai_types::ModelSelection>> {
        Ok(self.require_session(session_id).await?.selected_model)
    }

    pub async fn set_session_model(
        &self,
        session_id: &SessionId,
        selection: kraai_types::ModelSelection,
    ) -> Result<()> {
        if self.is_turn_active(session_id) {
            return Err(eyre!(kraai_types::DomainError::conflict(
                "Cannot change model while the current turn is active"
            )));
        }
        self.require_session(session_id).await?;
        self.validate_model_options(
            &selection.provider_id,
            &selection.model_id,
            &selection.options,
            false,
        )
        .await?;
        self.persist_model_selection(session_id, &selection).await
    }

    pub(super) async fn persist_model_selection(
        &self,
        session_id: &SessionId,
        selection: &kraai_types::ModelSelection,
    ) -> Result<()> {
        let mut session = self.require_session(session_id).await?;
        if session.selected_model.as_ref() != Some(selection) {
            session.selected_model = Some(selection.clone());
            session.updated_at = current_unix_timestamp();
            self.session_store.save(&session).await?;
        }
        Ok(())
    }

    pub async fn validate_model_options(
        &self,
        provider_id: &ProviderId,
        model_id: &ModelId,
        options: &ModelOptionValues,
        require_all: bool,
    ) -> Result<()> {
        let provider = read(&self.providers)
            .get_provider(provider_id)
            .ok_or_else(|| {
                eyre!(kraai_types::DomainError::not_found(format!(
                    "Unknown provider: {provider_id}"
                )))
            })?;
        let model = provider.get_model(model_id).await.ok_or_else(|| {
            eyre!(kraai_types::DomainError::not_found(format!(
                "Unknown model: {model_id}"
            )))
        })?;
        kraai_types::validate_model_option_values(&model.options, options, require_all).map_err(
            |errors| {
                eyre!(kraai_types::DomainError::invalid_argument(
                    errors
                        .into_iter()
                        .map(|error| format!("{}: {}", error.option, error.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                ))
            },
        )?;
        kraai_provider_core::validate_model_option_effects(&model.options, options).map_err(
            |error| {
                eyre!(kraai_types::DomainError::invalid_argument(
                    error.to_string()
                ))
            },
        )
    }

    pub async fn get_workspace_dir_state(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<(PathBuf, bool)>> {
        let Some(session) = self.session_store.get(session_id).await? else {
            return Ok(None);
        };

        Ok(Some(self.with_runtime_state(
            session_id,
            &session.workspace_dir,
            |state| {
                (
                    state.effective_workspace_dir(),
                    state.pending_workspace_dir.is_some(),
                )
            },
        )))
    }

    pub fn is_profile_locked(&self, session_id: &SessionId) -> bool {
        self.is_turn_active(session_id)
    }

    pub async fn set_providers(
        &self,
        config: ProviderManagerConfig,
        registry: ProviderRegistry,
    ) -> Result<()> {
        let mut providers = self.cloned_provider_manager();
        let result = providers.load_config(config, registry).await;
        *write(&self.providers) = providers;
        result
    }

    pub async fn list_models(&self) -> HashMap<ProviderId, Vec<Model>> {
        self.cloned_provider_manager().list_all_models().await
    }

    pub async fn get_tip(&self, session_id: &SessionId) -> Result<Option<MessageId>> {
        Ok(self
            .session_store
            .get(session_id)
            .await?
            .and_then(|session| session.tip_id))
    }

    #[cfg(test)]
    pub(super) async fn set_tip(
        &self,
        session_id: &SessionId,
        new_tip: Option<MessageId>,
    ) -> Result<()> {
        if let Some(mut session) = self.session_store.get(session_id).await? {
            session.tip_id = new_tip;
            session.updated_at = current_unix_timestamp();
            self.session_store.save(&session).await?;
        }

        Ok(())
    }

    pub(super) fn with_runtime_state<T>(
        &self,
        session_id: &SessionId,
        workspace_dir: &Path,
        update: impl FnOnce(&mut SessionRuntimeState) -> T,
    ) -> T {
        let mut states = lock(&self.session_states);
        let state = states
            .entry(session_id.clone())
            .or_insert_with(|| SessionRuntimeState::new(workspace_dir.to_path_buf()));
        if state.active_turn_profile.is_none() {
            state.active_workspace_dir = workspace_dir.to_path_buf();
            if state
                .pending_workspace_dir
                .as_deref()
                .is_some_and(|pending| pending != workspace_dir)
            {
                state.pending_workspace_dir = None;
            }
        }
        update(state)
    }

    pub(super) fn resolve_profiles_for_workspace(&self, workspace_dir: &Path) -> ResolvedProfiles {
        resolve_profiles(
            workspace_dir,
            &self.storage_root,
            &crate::profiles::available_command_ids(),
        )
    }

    pub(super) fn resolve_selected_profile(&self, session: &SessionMeta) -> Result<AgentProfile> {
        let Some(profile_id) = session.selected_profile_id.as_ref() else {
            return Err(eyre!(kraai_types::DomainError::conflict(
                "No profile selected for this session"
            )));
        };

        self.resolve_profiles_for_workspace(&session.workspace_dir)
            .profiles
            .into_iter()
            .find(|profile| &profile.id == profile_id)
            .ok_or_else(|| {
                eyre!(kraai_types::DomainError::not_found(format!(
                    "Selected profile is unavailable: {profile_id}"
                )))
            })
    }

    pub(super) async fn require_session(&self, session_id: &SessionId) -> Result<SessionMeta> {
        self.session_store.get(session_id).await?.ok_or_else(|| {
            eyre!(kraai_types::DomainError::not_found(format!(
                "Session not found: {session_id}"
            )))
        })
    }

    pub fn script_turn_context(&self, session_id: &SessionId) -> Result<ScriptTurnContext> {
        let states = lock(&self.session_states);
        let state = states.get(session_id).ok_or_else(|| {
            eyre!(kraai_types::DomainError::not_found(format!(
                "Session runtime state is unavailable: {session_id}"
            )))
        })?;
        let profile = state.active_turn_profile.as_ref().ok_or_else(|| {
            eyre!(kraai_types::DomainError::conflict(format!(
                "Session has no active turn: {session_id}"
            )))
        })?;
        Ok(ScriptTurnContext {
            workspace_dir: state.active_workspace_dir.clone(),
            profile: profile.snapshot(),
        })
    }
}
