use std::collections::{HashMap, HashSet, hash_map::Entry};

use color_eyre::eyre::Result;
use kraai_persistence::ScriptExecutionCompletion;
use kraai_types::{MessageId, ScriptExecutionPhase, ScriptExecutionStatus};

use super::core::RuntimeCore;
use super::script_execution::CompletedScriptExecution;

impl RuntimeCore {
    pub(crate) async fn recover_session_executions(&self, session_id: &str) -> Result<()> {
        if !self.session_store.owns_turn(session_id).await? {
            return Err(color_eyre::eyre::eyre!(
                "Session recovery requires turn ownership"
            ));
        }
        let records = self.execution_store.list_for_session(session_id).await?;
        if records.is_empty() {
            return Ok(());
        }
        let sessions = self.agent_manager.read().await.list_session_ids().await?;
        let mut sources: HashMap<String, HashSet<MessageId>> = HashMap::new();
        for record in &records {
            if sessions.contains(&record.session_id) {
                sources
                    .entry(record.session_id.clone())
                    .or_default()
                    .insert(record.source_message_id.clone());
            }
        }
        let mut histories: HashMap<String, HashSet<MessageId>> = HashMap::new();
        for record in records {
            if !sessions.contains(&record.session_id) {
                continue;
            }
            let history = match histories.entry(record.session_id.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(
                    self.agent_manager
                        .read()
                        .await
                        .reachable_message_ids(
                            &record.session_id,
                            sources.remove(&record.session_id).unwrap_or_default(),
                        )
                        .await?,
                ),
            };
            if !history.contains(&record.source_message_id) {
                continue;
            }
            let completed = if record.phase == kraai_types::ScriptExecutionPhase::Finished {
                CompletedScriptExecution {
                    output: self.execution_store.read_output(&record.id).await?,
                    record,
                }
            } else {
                let output = self.execution_store.read_output(&record.id).await?;
                let (status, error) = interrupted_execution_outcome(record.phase);
                let record = self
                    .execution_store
                    .finish(
                        &record.id,
                        ScriptExecutionCompletion {
                            status,
                            exit_code: None,
                            sandbox_denied: record.sandbox_denied,
                            error: Some(error),
                            stdout: output.stdout.clone(),
                            stderr: output.stderr.clone(),
                        },
                    )
                    .await?;
                CompletedScriptExecution { record, output }
            };

            let result = completed.render_result()?;
            let result_message_id = completed.record.result_message_id.clone();
            let session_id = completed.record.session_id.clone();
            self.agent_manager
                .write()
                .await
                .add_script_result_to_history(
                    &session_id,
                    result_message_id.clone(),
                    completed.record.profile.id.clone(),
                    completed.record.call_id.clone(),
                    result,
                    completed.record.outcome()?,
                )
                .await?;
        }
        Ok(())
    }
}

pub(super) fn interrupted_execution_outcome(
    phase: ScriptExecutionPhase,
) -> (ScriptExecutionStatus, String) {
    match phase {
        ScriptExecutionPhase::Prepared => (
            ScriptExecutionStatus::FailedToStart,
            String::from("Kraai stopped before the prepared script could start"),
        ),
        ScriptExecutionPhase::AwaitingApproval => (
            ScriptExecutionStatus::Cancelled,
            String::from("Kraai stopped while the script was awaiting approval; it was not run"),
        ),
        ScriptExecutionPhase::Running => (
            ScriptExecutionStatus::RuntimeError,
            String::from("Kraai stopped while the script was running"),
        ),
        ScriptExecutionPhase::Finished => (
            ScriptExecutionStatus::RuntimeError,
            String::from("Kraai found a finished script without a terminal outcome"),
        ),
    }
}
