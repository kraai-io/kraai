use kraai_runtime::TurnTimer;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use kraai_runtime::{
    AgentProfileSummary, AgentProfileWarning, Model, PendingScriptInfo, ProviderDefinition,
    Session, SessionContextUsage as RuntimeSessionContextUsage, SettingsDocument,
};
use kraai_types::{Message, MessageContent, MessageId, SessionId};

use super::auth::ProviderAuthStatus;
use super::chat_render::ChatRenderCache;
use super::types::{
    ExitUsageTotals, OptimisticMessage, PendingSubmit, ProvidersView, RunningScript,
    ScriptApprovalAction, ScriptPhase, UiMode,
};
use crate::components::{RunningCard, ScriptCard, running_dot};

pub(super) struct AppState {
    pub(super) session_cost: kraai_types::CostSummary,
    pub(super) session_requests:
        BTreeMap<SessionId, BTreeMap<MessageId, kraai_types::RequestUsage>>,
    pub(super) launch_requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
    pub(super) launched_at: u64,
    pub(super) cost_recovery_list_pending: bool,
    pub(super) cost_recovery_sessions: BTreeSet<SessionId>,
    pub(super) exit: bool,
    pub(super) sheet_query: super::line_edit::LineEdit,
    pub(super) sheet_index: usize,
    pub(super) sheet_scroll: Cell<usize>,
    pub(super) sheet_notice: Option<String>,
    pub(super) sheet_armed: bool,
    pub(super) sheet_feedback: Option<String>,
    pub(super) model_settings: Option<super::model_options::ModelSettingsTarget>,
    pub(super) command_filter: Option<String>,
    pub(super) approval_scroll: Cell<usize>,
    pub(super) approval_expanded: bool,
    pub(super) scripts_expanded: bool,
    pub(super) input: String,
    pub(super) draft_images: super::draft_images::DraftImages,
    pub(super) next_image_request_id: u64,
    pub(super) pending_messages: VecDeque<super::types::PendingMessage>,
    pub(super) failed_messages: HashMap<Option<SessionId>, Vec<MessageContent>>,
    pub(super) input_cursor: usize,
    pub(super) input_width: u16,
    pub(super) input_history: Vec<String>,
    pub(super) input_history_index: Option<usize>,
    pub(super) input_history_draft: Option<(String, super::draft_images::DraftImages)>,
    pub(super) ctrl_c_exit_armed: bool,
    pub(super) chat_history: BTreeMap<MessageId, Message>,
    pub(super) optimistic_messages: Vec<OptimisticMessage>,
    pub(super) optimistic_seq: u64,
    pub(super) chat_epoch: u64,
    pub(super) cards_epoch: u64,
    pub(super) chat_render_cache: RefCell<ChatRenderCache>,
    pub(super) chat_viewport_height: u16,
    pub(super) scroll: usize,
    pub(super) auto_scroll: bool,
    pub(super) config_loaded: bool,
    pub(super) mode: UiMode,
    pub(super) turn_cancelled: bool,
    pub(super) error_notice: Option<String>,
    pub(super) last_error: Option<String>,
    pub(super) error_open: bool,
    pub(super) error_scroll: Cell<usize>,
    pub(super) is_streaming: bool,
    pub(super) retry_waiting: bool,
    pub(super) turn_timer: TurnTimer,
    pub(super) feedback: super::feedback::VisualFeedback,
    pub(super) cursor_preferences: super::cursor_preferences::CursorPreferences,
    pub(super) palette: super::palette::TerminalPalette,
    pub(super) breath_origin: std::time::Instant,
    pub(super) models_by_provider: HashMap<String, Vec<Model>>,
    pub(super) agent_profiles: Vec<AgentProfileSummary>,
    pub(super) agent_profile_warnings: Vec<AgentProfileWarning>,
    pub(super) sandbox_warning: Option<String>,
    pub(super) provider_definitions: Vec<ProviderDefinition>,
    pub(super) selected_profile_id: Option<String>,
    pub(super) profile_locked: bool,
    pub(super) profile_lock_stale_after_terminal_event: bool,
    pub(super) selected_provider_id: Option<String>,
    pub(super) selected_model_id: Option<String>,
    pub(super) last_session_model: Option<kraai_types::ModelSelection>,
    pub(super) session_model_saves: HashMap<SessionId, super::session_model::SessionModelSave>,
    pub(super) next_model_save_id: u64,
    pub(super) selected_model_options: kraai_types::ModelOptionValues,
    pub(super) remembered_model_options:
        BTreeMap<String, BTreeMap<String, kraai_types::ModelOptionValues>>,
    pub(super) context_usage: Option<RuntimeSessionContextUsage>,
    pub(super) pending_script: Option<PendingScriptInfo>,
    pub(super) running_scripts: HashMap<String, RunningScript>,
    pub(super) script_clock: Option<std::time::Instant>,
    pub(super) submitted_script_decisions: BTreeMap<SessionId, BTreeSet<String>>,
    pub(super) sessions: Vec<Session>,
    pub(super) current_session_id: Option<SessionId>,
    pub(super) current_tip_id: Option<String>,
    pub(super) workspace_dir: String,
    pub(super) home_dir: String,
    pub(super) script_approval_action: ScriptApprovalAction,
    pub(super) script_phase: ScriptPhase,
    pub(super) command_popup_dismissed: bool,
    pub(super) settings_draft: Option<SettingsDocument>,
    pub(super) settings_errors: HashMap<String, String>,
    pub(super) settings_provider_index: usize,
    pub(super) providers_view: ProvidersView,
    pub(super) config_editing: Option<super::provider_config::ConfigRow>,
    pub(super) provider_auth: HashMap<String, ProviderAuthStatus>,
    pub(super) mcp_auth: BTreeMap<String, kraai_runtime::McpAuthStatus>,
    pub(super) mcp_browser_intent: BTreeMap<String, super::mcp_auth::BrowserIntent>,
    pub(super) next_mcp_login_request: u64,
    pub(super) mcp_open: Option<String>,
    pub(super) mcp_loaded: bool,
    pub(super) mcp_feedback: BTreeMap<String, String>,
    pub(super) mcp_error: Option<String>,
    pub(super) pending_submit: Option<PendingSubmit>,
    pub(super) next_session_creation_id: u64,
    pub(super) next_session_load_id: u64,
    pub(super) pending_session_load_id: Option<u64>,
    pub(super) exit_usage_totals: ExitUsageTotals,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            session_cost: Default::default(),
            cost_recovery_list_pending: false,
            cost_recovery_sessions: Default::default(),
            session_requests: BTreeMap::new(),
            launch_requests: BTreeMap::new(),
            launched_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            exit: false,
            sheet_query: super::line_edit::LineEdit::default(),
            sheet_index: 0,
            sheet_scroll: Cell::new(0),
            sheet_notice: None,
            sheet_armed: false,
            sheet_feedback: None,
            model_settings: None,
            command_filter: None,
            approval_scroll: Cell::new(0),
            approval_expanded: false,
            scripts_expanded: false,
            input: String::new(),
            draft_images: super::draft_images::DraftImages::default(),
            next_image_request_id: 1,
            pending_messages: VecDeque::new(),
            failed_messages: HashMap::new(),
            input_cursor: 0,
            input_width: 80,
            input_history: Vec::new(),
            input_history_index: None,
            input_history_draft: None,
            ctrl_c_exit_armed: false,
            chat_history: BTreeMap::new(),
            optimistic_messages: Vec::new(),
            optimistic_seq: 0,
            chat_epoch: 0,
            cards_epoch: 0,
            chat_render_cache: RefCell::new(ChatRenderCache::default()),
            chat_viewport_height: 0,
            scroll: 0,
            auto_scroll: true,
            config_loaded: false,
            mode: UiMode::Chat,
            turn_cancelled: false,
            error_notice: None,
            last_error: None,
            error_open: false,
            error_scroll: Cell::new(0),
            is_streaming: false,
            retry_waiting: false,
            turn_timer: TurnTimer::default(),
            feedback: super::feedback::VisualFeedback::default(),
            cursor_preferences: Default::default(),
            palette: super::palette::TerminalPalette::default(),
            breath_origin: std::time::Instant::now(),
            models_by_provider: HashMap::new(),
            agent_profiles: Vec::new(),
            agent_profile_warnings: Vec::new(),
            sandbox_warning: None,
            provider_definitions: Vec::new(),
            selected_profile_id: None,
            profile_locked: false,
            profile_lock_stale_after_terminal_event: false,
            selected_provider_id: None,
            selected_model_id: None,
            last_session_model: None,
            session_model_saves: HashMap::new(),
            next_model_save_id: 0,
            selected_model_options: Default::default(),
            remembered_model_options: BTreeMap::new(),
            context_usage: None,
            pending_script: None,
            running_scripts: HashMap::new(),
            script_clock: None,
            submitted_script_decisions: BTreeMap::new(),
            sessions: Vec::new(),
            current_session_id: None,
            current_tip_id: None,
            workspace_dir: String::new(),
            home_dir: String::new(),
            script_approval_action: ScriptApprovalAction::Allow,
            script_phase: ScriptPhase::Idle,
            command_popup_dismissed: false,
            settings_draft: None,
            settings_errors: HashMap::new(),
            settings_provider_index: 0,
            providers_view: ProvidersView::List,
            config_editing: None,
            provider_auth: HashMap::new(),
            mcp_auth: BTreeMap::new(),
            mcp_browser_intent: Default::default(),
            next_mcp_login_request: 0,
            mcp_open: None,
            mcp_loaded: false,
            mcp_feedback: BTreeMap::new(),
            mcp_error: None,
            pending_submit: None,
            next_session_creation_id: 0,
            next_session_load_id: 0,
            pending_session_load_id: None,
            exit_usage_totals: ExitUsageTotals::default(),
        }
    }
}

impl AppState {
    pub(super) fn from_startup_options(startup_options: &super::StartupOptions) -> Self {
        let workspace_preferences = super::workspace_preferences::load_for_current_workspace()
            .unwrap_or_else(|error| {
                tracing::warn!("Failed to load workspace preferences: {error}");
                super::WorkspacePreferences::default()
            });

        let selected_model_options = workspace_preferences
            .provider_id
            .as_ref()
            .zip(workspace_preferences.model_id.as_ref())
            .and_then(|(provider, model)| {
                workspace_preferences
                    .model_options
                    .get(provider)?
                    .get(model)
            })
            .cloned()
            .unwrap_or_default();

        Self {
            selected_provider_id: workspace_preferences.provider_id,
            selected_model_id: workspace_preferences.model_id,
            selected_model_options,
            remembered_model_options: workspace_preferences.model_options,
            selected_profile_id: startup_options
                .agent_profile_id
                .clone()
                .or(workspace_preferences.agent_profile_id),
            workspace_dir: super::workspace_preferences::current_workspace()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            home_dir: std::env::home_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            ..Self::default()
        }
    }

    pub(super) fn breath(&self, at: std::time::Instant) -> std::time::Duration {
        at.saturating_duration_since(self.breath_origin)
    }

    pub(super) fn script_card(&self, call_id: &str, writing: bool) -> ScriptCard {
        if let Some(script) = self.running_scripts.get(call_id) {
            let now = self.script_clock.unwrap_or(script.started);
            return ScriptCard::Running(RunningCard::new(
                now.saturating_duration_since(script.started),
                script.timeout,
                script.tail.clone(),
                running_dot(self.breath(now), self.palette.muted),
            ));
        }
        if self.script_phase == ScriptPhase::AwaitingApproval
            && self
                .pending_script
                .as_ref()
                .is_some_and(|script| script.call_id == call_id)
        {
            return ScriptCard::AwaitingApproval;
        }
        if writing {
            ScriptCard::Writing
        } else {
            ScriptCard::Waiting
        }
    }

    pub(super) fn has_local_script_approval(&self) -> bool {
        self.script_phase == ScriptPhase::AwaitingApproval
            && self.pending_script.is_some()
            && !self.script_decision_pending()
    }

    pub(super) fn script_decision_pending(&self) -> bool {
        self.current_session_id
            .as_ref()
            .and_then(|session_id| self.submitted_script_decisions.get(session_id))
            .zip(self.pending_script.as_ref())
            .is_some_and(|(executions, script)| executions.contains(&script.execution_id))
    }

    pub(super) fn runtime_is_active(&self) -> bool {
        self.is_streaming
            || self.retry_waiting
            || self.script_decision_pending()
            || self.script_phase == ScriptPhase::Executing
            || (self.profile_locked
                && !self.profile_lock_stale_after_terminal_event
                && !matches!(self.script_phase, ScriptPhase::AwaitingApproval))
    }

    pub(super) fn turn_blocks_user_commands(&self) -> bool {
        self.is_streaming
            || self.retry_waiting
            || self.script_phase == ScriptPhase::Executing
            || self.script_phase == ScriptPhase::AwaitingApproval
            || (self.profile_locked
                && !self.profile_lock_stale_after_terminal_event
                && self.script_phase != ScriptPhase::AwaitingApproval)
    }
}
