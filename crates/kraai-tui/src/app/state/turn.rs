use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use kraai_runtime::{PendingScriptInfo, SessionContextUsage, TurnTimer};
use kraai_types::SessionId;

use crate::app::types::{RunningScript, ScriptApprovalAction, ScriptPhase};

pub(in crate::app) struct Turn {
    pub(in crate::app) is_streaming: bool,
    pub(in crate::app) retry_waiting: bool,
    pub(in crate::app) timer: TurnTimer,
    pub(in crate::app) cancelled: bool,
    pub(in crate::app) context_usage: Option<SessionContextUsage>,
    pub(in crate::app) pending_script: Option<PendingScriptInfo>,
    pub(in crate::app) running_scripts: HashMap<String, RunningScript>,
    pub(in crate::app) script_clock: Option<std::time::Instant>,
    pub(in crate::app) submitted_script_decisions: BTreeMap<SessionId, BTreeSet<String>>,
    pub(in crate::app) script_approval_action: ScriptApprovalAction,
    pub(in crate::app) script_phase: ScriptPhase,
    pub(in crate::app) approval_scroll: Cell<usize>,
    pub(in crate::app) approval_expanded: bool,
    pub(in crate::app) profile_locked: bool,
    pub(in crate::app) profile_lock_stale_after_terminal_event: bool,
}

impl Default for Turn {
    fn default() -> Self {
        Self {
            is_streaming: false,
            retry_waiting: false,
            timer: TurnTimer::default(),
            cancelled: false,
            context_usage: None,
            pending_script: None,
            running_scripts: HashMap::new(),
            script_clock: None,
            submitted_script_decisions: BTreeMap::new(),
            script_approval_action: ScriptApprovalAction::Allow,
            script_phase: ScriptPhase::Idle,
            approval_scroll: Cell::new(0),
            approval_expanded: false,
            profile_locked: false,
            profile_lock_stale_after_terminal_event: false,
        }
    }
}

impl Turn {
    pub(in crate::app) fn has_local_approval(&self, session_id: Option<&SessionId>) -> bool {
        self.script_phase == ScriptPhase::AwaitingApproval
            && self.pending_script.is_some()
            && !self.decision_pending(session_id)
    }

    pub(in crate::app) fn decision_pending(&self, session_id: Option<&SessionId>) -> bool {
        session_id
            .and_then(|session_id| self.submitted_script_decisions.get(session_id))
            .zip(self.pending_script.as_ref())
            .is_some_and(|(executions, script)| executions.contains(&script.execution_id))
    }

    pub(in crate::app) fn is_active(&self, session_id: Option<&SessionId>) -> bool {
        self.is_streaming
            || self.retry_waiting
            || self.decision_pending(session_id)
            || self.script_phase == ScriptPhase::Executing
            || (self.profile_locked
                && !self.profile_lock_stale_after_terminal_event
                && !matches!(self.script_phase, ScriptPhase::AwaitingApproval))
    }

    pub(in crate::app) fn blocks_user_commands(&self) -> bool {
        self.is_streaming
            || self.retry_waiting
            || self.script_phase == ScriptPhase::Executing
            || self.script_phase == ScriptPhase::AwaitingApproval
            || (self.profile_locked
                && !self.profile_lock_stale_after_terminal_event
                && self.script_phase != ScriptPhase::AwaitingApproval)
    }
}
