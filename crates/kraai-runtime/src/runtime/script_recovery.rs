use color_eyre::eyre::Result;
use kraai_execution_types::{ScriptExecutionPhase, ScriptExecutionStatus};
use kraai_persistence::ScriptExecutionCompletion;
use kraai_types::SessionId;

use super::core::RuntimeCore;
use super::script_execution::CompletedScriptExecution;

impl RuntimeCore {
    pub(crate) async fn recover_session_executions(&self, session_id: &SessionId) -> Result<()> {
        if !self.session_store.owns_turn(session_id).await? {
            return Err(color_eyre::eyre::eyre!(
                "Session recovery requires turn ownership"
            ));
        }
        let records = self
            .execution_store
            .list_unsettled_for_session(session_id)
            .await?;
        if records.is_empty()
            || !self
                .agent_manager
                .list_session_ids()
                .await?
                .contains(session_id)
        {
            return Ok(());
        }
        let candidates = records
            .iter()
            .flat_map(|record| {
                [
                    record.source_message_id.clone(),
                    record.result_message_id.clone(),
                ]
            })
            .collect();
        let reachable = self
            .agent_manager
            .reachable_message_ids(session_id, candidates)
            .await?;
        for record in records {
            let id = record.id.clone();
            if reachable.contains(&record.source_message_id)
                && !reachable.contains(&record.result_message_id)
            {
                self.deliver_recovered_result(record).await?;
            }
            self.execution_store.mark_settled(&id).await?;
        }
        Ok(())
    }

    async fn deliver_recovered_result(
        &self,
        record: kraai_persistence::ScriptExecutionRecord,
    ) -> Result<()> {
        let stored = self
            .agent_manager
            .get_message(&record.result_message_id)
            .await?
            .and_then(|message| match message.content {
                kraai_types::ConversationItem::ScriptResult {
                    output, outcome, ..
                } => Some((output, outcome)),
                kraai_types::ConversationItem::FileContext { .. }
                | kraai_types::ConversationItem::Compaction { .. }
                | kraai_types::ConversationItem::System { .. }
                | kraai_types::ConversationItem::User { .. }
                | kraai_types::ConversationItem::Assistant { .. } => None,
            });
        if let Some((output, outcome)) = stored {
            let recorded = record.outcome().ok();
            if recorded.as_ref() != Some(&outcome) {
                tracing::warn!(
                    execution_id = ?record.id,
                    message_id = ?record.result_message_id,
                    ?recorded,
                    delivered = ?outcome,
                    "Delivered script result disagrees with its execution record"
                );
            }
            self.agent_manager
                .add_script_result_to_history(
                    &record.session_id,
                    record.result_message_id.clone(),
                    record.profile.id.clone(),
                    record.call_id.clone(),
                    output,
                    outcome,
                )
                .await?;
            return Ok(());
        }
        let output = self.execution_store.read_output(&record.id).await?;
        let completed = if record.phase == ScriptExecutionPhase::Finished {
            CompletedScriptExecution { output, record }
        } else {
            let (status, error) = interrupted_execution_outcome(record.phase);
            let record = self
                .execution_store
                .finish(
                    &record.id,
                    ScriptExecutionCompletion {
                        status,
                        exit_code: None,
                        sandbox_denied: record.sandbox_denied,
                        sandbox_hint: record.sandbox_hint,
                        error: Some(error),
                        stdout: output.stdout.clone(),
                        stderr: output.stderr.clone(),
                    },
                )
                .await?;
            CompletedScriptExecution { record, output }
        };
        let result = self.render_script_result(&completed).await?;
        self.agent_manager
            .add_script_result_to_history(
                &completed.record.session_id,
                completed.record.result_message_id.clone(),
                completed.record.profile.id.clone(),
                completed.record.call_id.clone(),
                result,
                completed.record.outcome()?,
            )
            .await?;
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
