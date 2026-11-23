use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;

use color_eyre::eyre::{Result, bail, eyre};
use kraai_runtime::{
    CreateSessionRequest, Event, PROTOCOL_VERSION, RuntimeHandle, RuntimeStartupState,
};
use kraai_types::{AssistantItem, ConversationItem, Message, MessageId, ScriptCall, SessionId};
use tokio::sync::broadcast::error::RecvError;

use crate::{HARNESS_METRICS_SCHEMA_VERSION, HarnessMetrics, UsageMetrics};

const HISTORY_PAGE: usize = 200;

pub struct RunOptions {
    pub provider_id: String,
    pub model_id: String,
    pub options: Vec<String>,
    pub agent_profile: String,
    pub message: String,
}

pub struct RunOutcome {
    pub metrics: HarnessMetrics,
    pub failure: Option<String>,
}

pub async fn run(
    runtime: &RuntimeHandle,
    options: RunOptions,
    output: &mut impl Write,
) -> Result<RunOutcome> {
    match runtime.wait_for_startup().await? {
        RuntimeStartupState::Ready { protocol_version } if protocol_version == PROTOCOL_VERSION => {
        }
        RuntimeStartupState::Ready { protocol_version } => {
            bail!("Runtime speaks protocol {protocol_version}, expected {PROTOCOL_VERSION}")
        }
        RuntimeStartupState::Failed(error) => bail!("Runtime failed to start: {error}"),
        RuntimeStartupState::Starting => bail!("Runtime did not finish starting"),
    }
    let models = runtime.list_models().await?;
    let model = models
        .get(&options.provider_id)
        .ok_or_else(|| eyre!("Unknown provider: {}", options.provider_id))?
        .iter()
        .find(|model| model.id == options.model_id)
        .ok_or_else(|| {
            eyre!(
                "Unknown model for provider {}: {}",
                options.provider_id,
                options.model_id
            )
        })?;
    let mut values = kraai_types::ModelOptionValues::new();
    kraai_types::reconcile_model_option_values(&model.options, &mut values);
    let values =
        kraai_types::parse_model_option_assignments(&model.options, &options.options, values)
            .map_err(|error| eyre!(error))?;

    let session = runtime
        .create_session_with(CreateSessionRequest {
            workspace_dir: None,
            profile_id: Some(options.agent_profile),
        })
        .await?;
    let mut events = runtime.subscribe_session(&session);
    runtime
        .send_message(
            session.clone(),
            options.message,
            options.model_id,
            options.provider_id,
            values,
        )
        .await?;

    let mut transcript = Transcript::default();
    let mut failure = None;
    loop {
        let event = match events.recv().await {
            Ok(event) => event.event,
            Err(RecvError::Lagged(skipped)) => {
                failure.get_or_insert_with(|| {
                    format!(
                        "Missed {skipped} runtime events; the transcript and metrics are incomplete"
                    )
                });
                let snapshot = runtime.get_session_snapshot(session.clone()).await?;
                if !snapshot.session.is_running {
                    break;
                }
                if snapshot.pending_script.is_some() {
                    runtime.cancel_turn(session.clone()).await?;
                }
                continue;
            }
            Err(RecvError::Closed) => {
                failure.get_or_insert_with(|| String::from("Runtime event stream closed"));
                break;
            }
        };
        match event {
            Event::ItemStarted {
                message_id,
                item_index,
                item,
                ..
            } => transcript.item(output, &message_id, item_index, &item)?,
            Event::ItemDelta {
                message_id,
                item_index,
                delta,
                ..
            } => transcript.text(output, &message_id, item_index, &delta)?,
            Event::StreamRestarted { .. } => {
                write!(
                    output,
                    "\n\n[provider stream failed; retrying the response]\n\n"
                )?;
            }
            Event::ProviderRetryScheduled {
                retry_number,
                delay_seconds,
                reason,
                ..
            } => {
                write!(
                    output,
                    "\n\n[{reason}, retrying in {delay_seconds} s (retry {retry_number})]\n\n"
                )?;
                output.flush()?;
            }
            Event::StreamComplete { message_id, .. } => {
                transcript.completed.insert(MessageId::new(message_id));
            }
            Event::ScriptApprovalRequested { .. } => {
                failure.get_or_insert_with(|| {
                    String::from(
                        "A headless run cannot answer a script capability escalation prompt",
                    )
                });
                runtime.cancel_turn(session.clone()).await?;
            }
            Event::StreamError { error, .. } => {
                failure.get_or_insert(format!("Stream error: {error}"));
            }
            Event::ContinuationFailed { error, .. } => {
                failure.get_or_insert(format!("Continuation failed: {error}"));
            }
            Event::SessionError { error, .. } => {
                failure.get_or_insert(format!("Session error: {error}"));
            }
            Event::ServiceError { error } => {
                failure.get_or_insert(format!("Runtime error: {error}"));
                break;
            }
            Event::SessionStateChanged {
                is_running: false, ..
            } => break,
            Event::TurnCompleted { .. }
            | Event::ScriptPrepared { .. }
            | Event::ScriptStarted { .. }
            | Event::ScriptOutputTail { .. }
            | Event::RequestUsageUpdated { .. }
            | Event::TurnTimingChanged { .. }
            | Event::ConfigLoaded
            | Event::ModelsUpdated
            | Event::StreamStart { .. }
            | Event::ReasoningDelta { .. }
            | Event::StreamCancelled { .. }
            | Event::ScriptResultReady { .. }
            | Event::ContextStateChanged { .. }
            | Event::SessionStateChanged {
                is_running: true, ..
            }
            | Event::MessageUpserted { .. }
            | Event::MessagesRemoved { .. }
            | Event::HistoryUpdated { .. }
            | Event::ProviderAuthUpdated { .. }
            | Event::McpAuthUpdated { .. } => {}
        }
    }
    output.flush()?;
    let metrics = collect_metrics(runtime, &session, &transcript.completed).await?;
    Ok(RunOutcome { metrics, failure })
}

#[derive(Default)]
struct Transcript {
    shown: HashMap<String, HashSet<usize>>,
    completed: HashSet<MessageId>,
}

impl Transcript {
    fn item(
        &mut self,
        output: &mut impl Write,
        message_id: &str,
        index: usize,
        item: &AssistantItem,
    ) -> Result<()> {
        match item {
            AssistantItem::Text { text, .. } => self.text(output, message_id, index, text),
            AssistantItem::ScriptCall(ScriptCall { input, .. }) => {
                self.shown
                    .entry(message_id.to_string())
                    .or_default()
                    .insert(index);
                write!(output, "\n\n```nu\n{input}\n```\n")?;
                Ok(())
            }
            AssistantItem::Reasoning { .. } => Ok(()),
        }
    }

    fn text(
        &mut self,
        output: &mut impl Write,
        message_id: &str,
        index: usize,
        text: &str,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let shown = self.shown.entry(message_id.to_string()).or_default();
        let earlier = shown.iter().any(|item| *item != index);
        if shown.insert(index) && earlier {
            output.write_all(b"\n\n")?;
        }
        output.write_all(text.as_bytes())?;
        Ok(())
    }
}

async fn collect_metrics(
    runtime: &RuntimeHandle,
    session: &SessionId,
    completed: &HashSet<MessageId>,
) -> Result<HarnessMetrics> {
    let history = read_history(runtime, session).await?;
    let mut usage = UsageMetrics::default();
    for message in history.values() {
        if completed.contains(&message.id)
            && let Some(reported) = message
                .generation
                .as_ref()
                .and_then(|generation| generation.usage.as_ref())
        {
            usage.accumulate(&UsageMetrics::from(reported));
        }
    }
    let script_executions = history
        .values()
        .filter(|message| matches!(message.content, ConversationItem::ScriptResult { .. }))
        .count();
    let final_context_tokens = runtime
        .get_session_context_usage(session.clone())
        .await?
        .map(|context| count(context.usage.used_context_tokens()));
    let requests = runtime
        .get_session_snapshot(session.clone())
        .await?
        .requests;
    let mut costs = kraai_types::CostSummary::default();
    for request in requests.values() {
        costs.add(request);
    }
    Ok(HarnessMetrics {
        schema_version: HARNESS_METRICS_SCHEMA_VERSION,
        turns: Some(count(completed.len())),
        script_executions: Some(count(script_executions)),
        final_context_tokens,
        usage: (usage != UsageMetrics::default()).then_some(usage),
        request_costs_complete: costs.unknown == 0 && !costs.overflow,
        request_costs: requests,
    })
}

async fn read_history(
    runtime: &RuntimeHandle,
    session: &SessionId,
) -> Result<BTreeMap<MessageId, Message>> {
    let mut history = BTreeMap::new();
    let mut from = None;
    loop {
        let page = runtime
            .get_history_page(session.clone(), from, HISTORY_PAGE)
            .await?;
        history.extend(page.messages);
        match page.older_history {
            Some(older) => from = Some(older),
            None => return Ok(history),
        }
    }
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
