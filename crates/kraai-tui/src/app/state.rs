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
    ActiveSettingsEditor, ExitUsageTotals, OptimisticMessage, PendingSubmit,
    ProvidersAdvancedFocus, ProvidersView, ScriptApprovalAction, ScriptPhase, SettingsFocus,
    UiMode,
};

pub(super) struct AppState {
    pub(super) session_cost: kraai_types::CostSummary,
    pub(super) session_requests:
        BTreeMap<SessionId, BTreeMap<MessageId, kraai_types::RequestUsage>>,
    pub(super) launch_requests: BTreeMap<MessageId, kraai_types::RequestUsage>,
    pub(super) launched_at: u64,
    pub(super) cost_recovery_list_pending: bool,
    pub(super) cost_recovery_sessions: std::collections::BTreeSet<SessionId>,
    pub(super) exit: bool,
    /// Text typed while a sheet owns the composer; the message draft stays in `input`.
    pub(super) sheet_query: String,
    pub(super) sheet_index: usize,
    pub(super) sheet_scroll: Cell<usize>,
    pub(super) sheet_notice: Option<String>,
    /// The highlighted row was asked to delete something and waits for the key a second time.
    pub(super) sheet_armed: bool,
    pub(super) model_settings: Option<super::model_options::ModelSettingsTarget>,
    /// What the command list stays filtered by while its rows are walked into the composer.
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
    pub(super) chat_render_cache: RefCell<ChatRenderCache>,
    pub(super) chat_viewport_height: u16,
    pub(super) scroll: usize,
    pub(super) auto_scroll: bool,
    pub(super) config_loaded: bool,
    pub(super) mode: UiMode,
    pub(super) status: String,
    pub(super) last_error: Option<String>,
    pub(super) error_open: bool,
    pub(super) error_scroll: Cell<usize>,
    pub(super) is_streaming: bool,
    pub(super) retry_waiting: bool,
    pub(super) turn_timer: TurnTimer,
    pub(super) feedback: super::feedback::VisualFeedback,
    pub(super) cursor_preferences: super::cursor_preferences::CursorPreferences,
    pub(super) palette: super::palette::TerminalPalette,
    pub(super) statusline_animation_frame: usize,
    pub(super) models_by_provider: HashMap<String, Vec<Model>>,
    pub(super) agent_profiles: Vec<AgentProfileSummary>,
    pub(super) agent_profile_warnings: Vec<AgentProfileWarning>,
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
    /// Option values last chosen for each provider and model.
    pub(super) remembered_model_options:
        BTreeMap<String, BTreeMap<String, kraai_types::ModelOptionValues>>,
    pub(super) context_usage: Option<RuntimeSessionContextUsage>,
    pub(super) pending_script: Option<PendingScriptInfo>,
    pub(super) submitted_script_decisions: BTreeMap<SessionId, BTreeSet<String>>,
    pub(super) sessions: Vec<Session>,
    pub(super) current_session_id: Option<SessionId>,
    pub(super) current_tip_id: Option<String>,
    /// Where Kraai runs, as sessions record it. Empty when it is unknown.
    pub(super) workspace_dir: String,
    pub(super) home_dir: String,
    pub(super) script_approval_action: ScriptApprovalAction,
    pub(super) script_phase: ScriptPhase,
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
    /// Sign-in state of each provider type that signs in with an account.
    pub(super) provider_auth: HashMap<String, ProviderAuthStatus>,
    pub(super) mcp_auth: BTreeMap<String, kraai_runtime::McpAuthStatus>,
    pub(super) mcp_browser_intent: BTreeMap<String, super::mcp_auth::BrowserIntent>,
    pub(super) next_mcp_login_request: u64,
    /// The server whose sheet is open. The server list shows while there is none.
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
            sheet_query: String::new(),
            sheet_index: 0,
            sheet_scroll: Cell::new(0),
            sheet_notice: None,
            sheet_armed: false,
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
            chat_render_cache: RefCell::new(ChatRenderCache::default()),
            chat_viewport_height: 0,
            scroll: 0,
            auto_scroll: true,
            config_loaded: false,
            mode: UiMode::Chat,
            status: String::from("Type / for commands"),
            last_error: None,
            error_open: false,
            error_scroll: Cell::new(0),
            is_streaming: false,
            retry_waiting: false,
            turn_timer: TurnTimer::default(),
            feedback: super::feedback::VisualFeedback::default(),
            cursor_preferences: Default::default(),
            palette: super::palette::TerminalPalette::default(),
            statusline_animation_frame: 0,
            models_by_provider: HashMap::new(),
            agent_profiles: Vec::new(),
            agent_profile_warnings: Vec::new(),
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
