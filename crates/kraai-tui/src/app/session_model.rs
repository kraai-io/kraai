use kraai_runtime::{RuntimeErrorKind, RuntimeResult, SessionSnapshot};
use kraai_types::{ModelId, ModelSelection, ProviderId};

use super::*;

#[derive(Default)]
pub(super) struct SessionModelSave {
    desired: Option<ModelSelection>,
    in_flight: Option<ModelSaveAttempt>,
    last_saved_id: u64,
    is_running: Option<bool>,
    failed: bool,
}

struct ModelSaveAttempt {
    id: u64,
    selection: ModelSelection,
}

impl App {
    pub(super) fn save_model_selection(&mut self) {
        self.reconcile_model_options();
        self.save_workspace_preferences();
        let Some(session_id) = self.state.current_session_id.clone() else {
            return;
        };
        let Some(selection) = self.current_model_selection() else {
            return;
        };
        let needs_snapshot = self.defer_session_model_selection(&session_id, selection);
        self.flush_session_model_save(&session_id);
        if needs_snapshot {
            self.request(RuntimeRequest::GetSessionSnapshot { session_id });
        }
    }

    fn current_model_selection(&mut self) -> Option<ModelSelection> {
        let provider = self.state.selected_provider_id.clone()?;
        let model = self.state.selected_model_id.clone()?;
        match (ProviderId::try_new(provider), ModelId::try_new(model)) {
            (Ok(provider_id), Ok(model_id)) => Some(ModelSelection {
                provider_id,
                model_id,
                options: self.state.selected_model_options.clone(),
            }),
            _ => {
                self.set_error(String::from("Cannot save an invalid model selection"));
                None
            }
        }
    }

    fn defer_session_model_selection(
        &mut self,
        session_id: &str,
        selection: ModelSelection,
    ) -> bool {
        let save = self
            .state
            .session_model_saves
            .entry(session_id.to_owned())
            .or_default();
        save.desired = Some(selection);
        save.failed = false;
        save.is_running.is_none()
    }

    pub(super) fn preserve_model_selection_after_creation(&mut self, pending: &PendingSubmit) {
        let Some(session_id) = pending.session_id.as_deref() else {
            return;
        };
        let Some(selection) = self.current_model_selection() else {
            return;
        };
        if selection.provider_id.as_str() != pending.provider_id
            || selection.model_id.as_str() != pending.model_id
            || selection.options != pending.options
        {
            self.defer_session_model_selection(session_id, selection);
        }
    }

    pub(super) fn reconcile_pending_session_model_options(&mut self) {
        let mut sessions: Vec<_> = self.state.session_model_saves.keys().cloned().collect();
        sessions.sort_unstable();
        for session_id in sessions {
            let Some(save) = self.state.session_model_saves.get_mut(&session_id) else {
                continue;
            };
            let Some(selection) = save.desired.as_mut() else {
                continue;
            };
            let Some(model) = self
                .state
                .models_by_provider
                .get(selection.provider_id.as_str())
                .and_then(|models| {
                    models
                        .iter()
                        .find(|model| model.id == selection.model_id.as_str())
                })
            else {
                continue;
            };
            let previous_options = selection.options.clone();
            kraai_types::reconcile_model_option_values(&model.options, &mut selection.options);
            if selection.options != previous_options {
                save.failed = false;
                self.flush_session_model_save(&session_id);
            }
        }
    }

    pub(super) fn flush_session_model_save(&mut self, session_id: &str) {
        if self
            .state
            .pending_messages
            .iter()
            .any(|message| message.session_id == session_id)
            || self
                .state
                .pending_submit
                .as_ref()
                .is_some_and(|pending| pending.session_id.as_deref() == Some(session_id))
            || (self.state.current_session_id.as_deref() == Some(session_id)
                && (self.state.runtime_is_active()
                    || self.state.pending_script.is_some()
                    || self.state.script_phase != ScriptPhase::Idle))
        {
            return;
        }
        let Some(save) = self.state.session_model_saves.get_mut(session_id) else {
            return;
        };
        if save.in_flight.is_some() || save.failed || save.is_running != Some(false) {
            return;
        }
        let Some(selection) = save.desired.clone() else {
            return;
        };
        self.state.next_model_save_id = self.state.next_model_save_id.saturating_add(1);
        let save_id = self.state.next_model_save_id;
        save.in_flight = Some(ModelSaveAttempt {
            id: save_id,
            selection: selection.clone(),
        });
        if self.request(RuntimeRequest::SetSessionModel {
            session_id: session_id.to_owned(),
            save_id,
            selection,
        }) == RuntimeRequestDelivery::Disconnected
            && let Some(save) = self.state.session_model_saves.get_mut(session_id)
        {
            save.in_flight = None;
        }
    }

    pub(super) fn handle_session_model_save(
        &mut self,
        session_id: &str,
        save_id: u64,
        result: RuntimeResult<()>,
    ) {
        let Some(save) = self.state.session_model_saves.get_mut(session_id) else {
            return;
        };
        if !save
            .in_flight
            .as_ref()
            .is_some_and(|attempt| attempt.id == save_id)
        {
            return;
        }
        let Some(attempt) = save.in_flight.take() else {
            return;
        };
        let is_foreground = self.state.current_session_id.as_deref() == Some(session_id);
        match result {
            Ok(()) => {
                save.last_saved_id = save_id;
                if save.desired.as_ref() == Some(&attempt.selection) {
                    save.desired = None;
                }
                if is_foreground {
                    self.state.last_session_model = Some(attempt.selection);
                }
                self.flush_session_model_save(session_id);
                self.request(RuntimeRequest::GetSessionSnapshot {
                    session_id: session_id.to_owned(),
                });
                self.request(RuntimeRequest::ListSessions);
            }
            Err(error) => {
                if error.kind == RuntimeErrorKind::Conflict {
                    save.is_running = None;
                    self.request(RuntimeRequest::GetSessionSnapshot {
                        session_id: session_id.to_owned(),
                    });
                } else if matches!(
                    error.kind,
                    RuntimeErrorKind::Unavailable | RuntimeErrorKind::Internal
                ) {
                    save.is_running = None;
                    if is_foreground {
                        self.set_error(format!("Failed saving session model: {error}"));
                    }
                } else if save.desired.as_ref() != Some(&attempt.selection) {
                    self.flush_session_model_save(session_id);
                } else {
                    save.failed = true;
                    if is_foreground {
                        self.set_error(format!("Failed saving session model: {error}"));
                    }
                }
            }
        }
    }

    pub(super) fn observe_session_model_snapshot(
        &mut self,
        session_id: &str,
        model_save_id: u64,
        snapshot: &SessionSnapshot,
    ) {
        let save = self
            .state
            .session_model_saves
            .entry(session_id.to_owned())
            .or_default();
        save.is_running = Some(
            snapshot.session.is_running
                || snapshot.activity != kraai_runtime::SessionActivity::Idle
                || snapshot.queued_messages != 0,
        );
        if self.state.current_session_id.as_deref() != Some(session_id)
            || model_save_id < save.last_saved_id
            || self.state.last_session_model == snapshot.session.selected_model
        {
            return;
        }
        self.state.last_session_model = snapshot.session.selected_model.clone();
        if save.desired.is_some() || save.in_flight.is_some() {
            return;
        }
        if let Some(selection) = &snapshot.session.selected_model {
            self.state.selected_provider_id = Some(selection.provider_id.to_string());
            self.state.selected_model_id = Some(selection.model_id.to_string());
            self.state.selected_model_options = selection.options.clone();
            self.reconcile_model_options();
        }
    }

    pub(super) fn mark_session_model_active(&mut self, session_id: &str) {
        self.state
            .session_model_saves
            .entry(session_id.to_owned())
            .or_default()
            .is_running = Some(true);
    }

    pub(super) fn refresh_session_model_saves(&mut self) {
        let sessions = self
            .state
            .sessions
            .iter()
            .filter(|session| {
                !session.is_running && !session.is_streaming && !session.waiting_for_approval
            })
            .filter(|session| {
                self.state
                    .session_model_saves
                    .get(&session.id)
                    .is_some_and(|save| {
                        save.desired.is_some() && save.in_flight.is_none() && !save.failed
                    })
            })
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        for session_id in sessions {
            self.request(RuntimeRequest::GetSessionSnapshot { session_id });
        }
    }

    pub(super) fn restore_pending_session_model(&mut self) {
        let Some(selection) = self
            .state
            .current_session_id
            .as_ref()
            .and_then(|session| self.state.session_model_saves.get(session))
            .and_then(|save| save.desired.clone())
        else {
            return;
        };
        self.state.selected_provider_id = Some(selection.provider_id.to_string());
        self.state.selected_model_id = Some(selection.model_id.to_string());
        self.state.selected_model_options = selection.options;
    }
}
