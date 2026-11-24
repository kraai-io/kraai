use super::workspace_preferences::WorkspacePreferences;
use crate::components::{RunningCard, ScriptCard, running_dot};

use super::types::{ScriptPhase, UiMode};

mod chat;
mod composer;
mod errors;
mod models;
mod providers;
mod sessions;
mod sheets;
mod turn;
mod usage;

pub(super) use chat::ChatView;
pub(super) use composer::Composer;
pub(super) use errors::Errors;
pub(super) use models::{Agents, Models};
pub(super) use providers::{Mcp, Providers};
pub(super) use sessions::Sessions;
pub(super) use sheets::Sheets;
pub(super) use turn::Turn;
pub(super) use usage::Usage;

pub(super) struct AppState {
    pub(super) exit: bool,
    pub(super) mode: UiMode,
    pub(super) config_loaded: bool,
    pub(super) ctrl_c_exit_armed: bool,
    pub(super) current_session_id: Option<kraai_types::SessionId>,
    pub(super) feedback: super::feedback::VisualFeedback,
    pub(super) cursor_preferences: super::cursor_preferences::CursorPreferences,
    pub(super) palette: super::palette::TerminalPalette,
    pub(super) breath_origin: std::time::Instant,
    pub(super) chat: ChatView,
    pub(super) turn: Turn,
    pub(super) composer: Composer,
    pub(super) sheet: Sheets,
    pub(super) sessions: Sessions,
    pub(super) models: Models,
    pub(super) agents: Agents,
    pub(super) providers: Providers,
    pub(super) mcp: Mcp,
    pub(super) usage: Usage,
    pub(super) errors: Errors,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            exit: false,
            mode: UiMode::Chat,
            config_loaded: false,
            ctrl_c_exit_armed: false,
            current_session_id: None,
            feedback: super::feedback::VisualFeedback::default(),
            cursor_preferences: Default::default(),
            palette: super::palette::TerminalPalette::default(),
            breath_origin: std::time::Instant::now(),
            chat: ChatView::default(),
            turn: Turn::default(),
            composer: Composer::default(),
            sheet: Sheets::default(),
            sessions: Sessions::default(),
            models: Models::default(),
            agents: Agents::default(),
            providers: Providers::default(),
            mcp: Mcp::default(),
            usage: Usage::default(),
            errors: Errors::default(),
        }
    }
}

impl AppState {
    pub(super) fn from_startup_options(startup_options: &super::StartupOptions) -> Self {
        let workspace_preferences = super::workspace_preferences::load_for_current_workspace()
            .unwrap_or_else(|error| {
                tracing::warn!("Failed to load workspace preferences: {error}");
                WorkspacePreferences::default()
            });

        let selected_options = workspace_preferences
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
            models: Models {
                selected_provider_id: workspace_preferences.provider_id,
                selected_model_id: workspace_preferences.model_id,
                selected_options,
                remembered_options: workspace_preferences.model_options,
                ..Models::default()
            },
            agents: Agents {
                selected_profile_id: startup_options
                    .agent_profile_id
                    .clone()
                    .or(workspace_preferences.agent_profile_id),
                ..Agents::default()
            },
            sessions: Sessions {
                workspace_dir: super::workspace_preferences::current_workspace()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                home_dir: std::env::home_dir()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                ..Sessions::default()
            },
            ..Self::default()
        }
    }

    pub(super) fn breath(&self, at: std::time::Instant) -> std::time::Duration {
        at.saturating_duration_since(self.breath_origin)
    }

    pub(super) fn script_card(&self, call_id: &str, writing: bool) -> ScriptCard {
        if let Some(script) = self.turn.running_scripts.get(call_id) {
            let now = self.turn.script_clock.unwrap_or(script.started);
            return ScriptCard::Running(RunningCard::new(
                now.saturating_duration_since(script.started),
                script.timeout,
                script.tail.clone(),
                running_dot(self.breath(now), self.palette.muted),
            ));
        }
        if self.turn.script_phase == ScriptPhase::AwaitingApproval
            && self
                .turn
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
        self.turn
            .has_local_approval(self.current_session_id.as_ref())
    }

    pub(super) fn script_decision_pending(&self) -> bool {
        self.turn.decision_pending(self.current_session_id.as_ref())
    }

    pub(super) fn runtime_is_active(&self) -> bool {
        self.turn.is_active(self.current_session_id.as_ref())
    }
}
