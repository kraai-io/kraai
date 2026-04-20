use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use kraai_runtime::{
    AgentProfileSummary, AgentProfileWarning, Model, ProviderDefinition, Session,
    SessionContextUsage as RuntimeSessionContextUsage, SettingsDocument,
};
use kraai_types::{ChatRole, Message, MessageId, MessageStatus};

use crate::components::{ChatHistory, RenderedLine, VisibleChatView};

use super::auth::ProviderAuthStatus;
use super::types::{
    ActiveSettingsEditor, ChatSelection, DEFAULT_AGENT_PROFILE_ID, ExitUsageTotals,
    OptimisticMessage, OptimisticToolMessage, PendingSubmit, PendingTool, ProvidersAdvancedFocus,
    ProvidersView, SettingsFocus, ToolApprovalAction, ToolPhase, UiMode, default_agent_profiles,
};

pub(super) struct AppState {
    pub(super) exit: bool,
    pub(super) input: String,
    pub(super) input_cursor: usize,
    pub(super) ctrl_c_exit_armed: bool,
    pub(super) chat_history: BTreeMap<MessageId, Message>,
    pub(super) optimistic_messages: Vec<OptimisticMessage>,
    pub(super) optimistic_tool_messages: Vec<OptimisticToolMessage>,
    pub(super) optimistic_seq: u64,
    pub(super) chat_epoch: u64,
    pub(super) chat_render_cache: RefCell<ChatRenderCache>,
    pub(super) chat_viewport_height: u16,
    pub(super) scroll: u16,
    pub(super) auto_scroll: bool,
    pub(super) selection: Option<ChatSelection>,
    pub(super) visible_chat_view: Option<VisibleChatView>,
    pub(super) config_loaded: bool,
    pub(super) mode: UiMode,
    pub(super) status: String,
    pub(super) is_streaming: bool,
    pub(super) retry_waiting: bool,
    pub(super) statusline_animation_frame: usize,
    pub(super) models_by_provider: HashMap<String, Vec<Model>>,
    pub(super) agent_profiles: Vec<AgentProfileSummary>,
    pub(super) agent_profile_warnings: Vec<AgentProfileWarning>,
    pub(super) provider_definitions: Vec<ProviderDefinition>,
    pub(super) selected_profile_id: Option<String>,
    pub(super) profile_locked: bool,
    pub(super) selected_provider_id: Option<String>,
    pub(super) selected_model_id: Option<String>,
    pub(super) context_usage: Option<RuntimeSessionContextUsage>,
    pub(super) pending_tools: Vec<PendingTool>,
    pub(super) sessions: Vec<Session>,
    pub(super) current_session_id: Option<String>,
    pub(super) current_tip_id: Option<String>,
    pub(super) agent_menu_index: usize,
    pub(super) model_menu_index: usize,
    pub(super) sessions_menu_index: usize,
    pub(super) tool_approval_action: ToolApprovalAction,
    pub(super) tool_phase: ToolPhase,
    pub(super) tool_batch_execution_started: bool,
    pub(super) command_completion_prefix: Option<String>,
    pub(super) command_completion_index: usize,
    pub(super) command_popup_dismissed: bool,
    pub(super) settings_draft: Option<SettingsDocument>,
    pub(super) settings_errors: HashMap<String, String>,
    pub(super) settings_focus: SettingsFocus,
    pub(super) settings_provider_index: usize,
    pub(super) settings_model_index: usize,
    pub(super) settings_provider_field_index: usize,
    pub(super) settings_model_field_index: usize,
    pub(super) settings_editor: Option<ActiveSettingsEditor>,
    pub(super) settings_editor_input: String,
    pub(super) settings_delete_armed: bool,
    pub(super) providers_view: ProvidersView,
    pub(super) providers_advanced_focus: ProvidersAdvancedFocus,
    pub(super) connect_provider_search: String,
    pub(super) connect_provider_index: usize,
    pub(super) openai_codex_auth: ProviderAuthStatus,
    pub(super) pending_submit: Option<PendingSubmit>,
    pub(super) exit_usage_totals: ExitUsageTotals,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            exit: false,
            input: String::new(),
            input_cursor: 0,
            ctrl_c_exit_armed: false,
            chat_history: BTreeMap::new(),
            optimistic_messages: Vec::new(),
            optimistic_tool_messages: Vec::new(),
            optimistic_seq: 0,
            chat_epoch: 0,
            chat_render_cache: RefCell::new(ChatRenderCache::default()),
            chat_viewport_height: 0,
            scroll: 0,
            auto_scroll: true,
            selection: None,
            visible_chat_view: None,
            config_loaded: false,
            mode: UiMode::Chat,
            status: String::from("Type /help for commands"),
            is_streaming: false,
            retry_waiting: false,
            statusline_animation_frame: 0,
            models_by_provider: HashMap::new(),
            agent_profiles: default_agent_profiles(),
            agent_profile_warnings: Vec::new(),
            provider_definitions: Vec::new(),
            selected_profile_id: Some(String::from(DEFAULT_AGENT_PROFILE_ID)),
            profile_locked: false,
            selected_provider_id: None,
            selected_model_id: None,
            context_usage: None,
            pending_tools: Vec::new(),
            sessions: Vec::new(),
            current_session_id: None,
            current_tip_id: None,
            agent_menu_index: 0,
            model_menu_index: 0,
            sessions_menu_index: 0,
            tool_approval_action: ToolApprovalAction::Allow,
            tool_phase: ToolPhase::Idle,
            tool_batch_execution_started: false,
            command_completion_prefix: None,
            command_completion_index: 0,
            command_popup_dismissed: false,
            settings_draft: None,
            settings_errors: HashMap::new(),
            settings_focus: SettingsFocus::ProviderList,
            settings_provider_index: 0,
            settings_model_index: 0,
            settings_provider_field_index: 0,
            settings_model_field_index: 0,
            settings_editor: None,
            settings_editor_input: String::new(),
            settings_delete_armed: false,
            providers_view: ProvidersView::List,
            providers_advanced_focus: ProvidersAdvancedFocus::ProviderFields,
            connect_provider_search: String::new(),
            connect_provider_index: 0,
            openai_codex_auth: ProviderAuthStatus::default(),
            pending_submit: None,
            exit_usage_totals: ExitUsageTotals::default(),
        }
    }
}

impl AppState {
    pub(super) fn from_startup_options(startup_options: super::StartupOptions) -> Self {
        Self {
            selected_provider_id: startup_options.provider_id,
            selected_model_id: startup_options.model_id,
            selected_profile_id: startup_options
                .agent_profile_id
                .or_else(|| Some(String::from(DEFAULT_AGENT_PROFILE_ID))),
            ..Self::default()
        }
    }

    pub(super) fn runtime_is_active(&self) -> bool {
        self.is_streaming
            || self.retry_waiting
            || self.tool_phase == ToolPhase::ExecutingBatch
            || (self.profile_locked && self.tool_phase != ToolPhase::Deciding)
    }

    pub(super) fn chat_max_scroll(&self) -> u16 {
        let cache = self.chat_render_cache.borrow();
        cache.total_lines.saturating_sub(self.chat_viewport_height)
    }

    pub(super) fn rendered_messages(&self) -> Vec<Message> {
        let mut rendered_messages: Vec<Message> =
            build_tip_chain(&self.chat_history, self.current_tip_id.as_deref())
                .into_iter()
                .cloned()
                .collect();

        for optimistic in &self.optimistic_messages {
            let content = if optimistic.is_queued {
                format!("{} [queued]", optimistic.content)
            } else {
                optimistic.content.clone()
            };
            rendered_messages.push(Message {
                id: MessageId::new(optimistic.local_id.clone()),
                parent_id: None,
                role: ChatRole::User,
                content,
                status: MessageStatus::Complete,
                agent_profile_id: self.selected_profile_id.clone(),
                tool_state_snapshot: None,
                tool_state_deltas: Vec::new(),
                generation: None,
            });
        }

        for optimistic in &self.optimistic_tool_messages {
            rendered_messages.push(Message {
                id: MessageId::new(optimistic.local_id.clone()),
                parent_id: None,
                role: ChatRole::Tool,
                content: optimistic.content.clone(),
                status: MessageStatus::Complete,
                agent_profile_id: self.selected_profile_id.clone(),
                tool_state_snapshot: None,
                tool_state_deltas: Vec::new(),
                generation: None,
            });
        }

        rendered_messages
    }

    pub(super) fn refresh_chat_render_cache(&self, width: u16) {
        let needs_refresh = {
            let cache = self.chat_render_cache.borrow();
            cache.epoch != self.chat_epoch || cache.width != width
        };
        if !needs_refresh {
            return;
        }

        let rendered_messages = self.rendered_messages();
        let mut cache = self.chat_render_cache.borrow_mut();
        let mut prior_entries = std::mem::take(&mut cache.message_cache);
        if cache.width != width {
            prior_entries.clear();
        }

        let mut next_entries: HashMap<String, CachedMessageRender> = HashMap::new();
        let mut sections = Vec::new();
        let mut total_lines: u16 = 0;

        for msg in &rendered_messages {
            let key = msg.id.as_str().to_string();
            let fingerprint = message_fingerprint(msg);
            let lines = match prior_entries.remove(&key) {
                Some(entry) if entry.fingerprint == fingerprint => entry.lines,
                _ => Arc::new(ChatHistory::build_message_lines(msg, width)),
            };

            if lines.is_empty() {
                continue;
            }

            if !sections.is_empty() {
                sections.push(Arc::new(vec![ChatHistory::separator_line()]));
                total_lines = total_lines.saturating_add(1);
            }

            total_lines = total_lines.saturating_add(lines.len().min(u16::MAX as usize) as u16);
            sections.push(Arc::clone(&lines));
            next_entries.insert(key, CachedMessageRender { fingerprint, lines });
        }

        cache.sections = sections;
        cache.total_lines = total_lines;
        cache.message_cache = next_entries;
        cache.width = width;
        cache.epoch = self.chat_epoch;
    }
}

#[derive(Default)]
pub(super) struct ChatRenderCache {
    pub(super) width: u16,
    pub(super) epoch: u64,
    pub(super) sections: Vec<Arc<Vec<RenderedLine>>>,
    pub(super) total_lines: u16,
    pub(super) message_cache: HashMap<String, CachedMessageRender>,
}

pub(super) struct CachedMessageRender {
    pub(super) fingerprint: u64,
    pub(super) lines: Arc<Vec<RenderedLine>>,
}

pub(super) fn build_tip_chain<'a>(
    history: &'a BTreeMap<MessageId, Message>,
    current_tip_id: Option<&str>,
) -> Vec<&'a Message> {
    if history.is_empty() {
        return Vec::new();
    }

    let mut parent_ids: HashSet<&MessageId> = HashSet::new();
    for msg in history.values() {
        if let Some(parent_id) = &msg.parent_id {
            parent_ids.insert(parent_id);
        }
    }

    let inferred_tip = history
        .keys()
        .find(|id| !parent_ids.contains(*id))
        .map(ToString::to_string);

    let current_tip_is_leaf = current_tip_id.is_some_and(|id| {
        let message_id = MessageId::new(id.to_string());
        history.contains_key(&message_id) && !parent_ids.contains(&message_id)
    });

    let tip_id = if current_tip_is_leaf {
        current_tip_id.map(|id| MessageId::new(id.to_string()))
    } else {
        inferred_tip.map(MessageId::new)
    };

    let mut chain = Vec::new();
    let mut cursor = tip_id;

    while let Some(message_id) = cursor {
        if let Some(message) = history.get(&message_id) {
            chain.push(message);
            cursor = message.parent_id.clone();
        } else {
            break;
        }
    }

    chain.reverse();
    chain
}

fn message_fingerprint(msg: &Message) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    msg.id.as_str().hash(&mut hasher);
    msg.parent_id
        .as_ref()
        .map(|id| id.as_str())
        .hash(&mut hasher);
    match msg.role {
        ChatRole::System => 0u8,
        ChatRole::User => 1u8,
        ChatRole::Assistant => 2u8,
        ChatRole::Tool => 3u8,
    }
    .hash(&mut hasher);
    match &msg.status {
        MessageStatus::Complete => 0u8.hash(&mut hasher),
        MessageStatus::Streaming { call_id } => {
            1u8.hash(&mut hasher);
            call_id.as_str().hash(&mut hasher);
        }
        MessageStatus::ProcessingTools => 2u8.hash(&mut hasher),
        MessageStatus::Cancelled => 3u8.hash(&mut hasher),
    }
    msg.content.hash(&mut hasher);
    hasher.finish()
}
