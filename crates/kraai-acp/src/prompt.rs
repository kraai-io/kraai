use std::collections::{HashMap, HashSet};

use agent_client_protocol::{Result, schema::v1 as acp};
use kraai_runtime::{Event, PendingScriptInfo, RuntimeHandle};
use kraai_types::{AssistantItem, ConversationItem, MessageId, SessionId};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::transport::Connection;
use crate::{commands, content, error, session::ActiveTurn};

#[cfg(test)]
mod tests;

pub(crate) async fn run(
    runtime: &RuntimeHandle,
    turn: ActiveTurn,
    request: acp::PromptRequest,
    connection: Connection,
) -> Result<acp::PromptResponse> {
    let cancelled = &turn.token;
    if cancelled.is_cancelled() {
        return Ok(acp::PromptResponse::new(acp::StopReason::Cancelled));
    }
    let command_response = || {
        acp::PromptResponse::new(if cancelled.is_cancelled() {
            acp::StopReason::Cancelled
        } else {
            acp::StopReason::EndTurn
        })
    };
    let id = crate::session::runtime_id(&request.session_id)?;
    let mut events = runtime.subscribe_session(&id);
    let model = crate::session::selected_model(runtime, &id).await?;
    let prompt_start = runtime
        .get_tip(id.clone())
        .await
        .map_err(error::runtime)?
        .map(MessageId::new);
    match commands::parse(&request.prompt)? {
        Some(commands::Command::Option {
            id: option_id,
            value,
        }) => {
            let mut publication = crate::config_updates::lock(&turn.session).await;
            crate::config::set_model_option(runtime, &id, &option_id, &value).await?;
            let config = crate::config::options(runtime, &id).await?;
            crate::config::notify(&connection, request.session_id, config.clone())?;
            *publication.current = Some(config);
            return Ok(command_response());
        }
        Some(commands::Command::Undo) => {
            commands::undo(runtime, &id, &connection).await?;
            return Ok(command_response());
        }
        None => {
            let message = content::prompt(runtime, request.prompt).await?;
            if cancelled.is_cancelled() {
                return Ok(acp::PromptResponse::new(acp::StopReason::Cancelled));
            }
            runtime
                .send_content(
                    id.clone(),
                    message,
                    model.model,
                    model.provider,
                    model.options,
                )
                .await
                .map_err(error::runtime)?;
        }
    }
    finish_prompt(
        runtime,
        &mut events,
        Output::new(id, connection, prompt_start),
        cancelled,
    )
    .await
}

async fn finish_prompt(
    runtime: &RuntimeHandle,
    events: &mut broadcast::Receiver<kraai_runtime::RuntimeEvent>,
    mut output: Output,
    cancelled: &CancellationToken,
) -> Result<acp::PromptResponse> {
    let result = drive(runtime, events, &mut output, cancelled).await;
    if cancelled.is_cancelled() || result.is_err() {
        runtime
            .cancel_turn(output.session_id.clone())
            .await
            .map_err(error::runtime)?;
        loop {
            match events.try_recv() {
                Ok(event) => {
                    if output.uncovered(&event) {
                        output.event(runtime, event.event).await?;
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    output.resync(runtime).await?;
                    break;
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => {
                    return Err(crate::error::internal(
                        "Runtime event stream interrupted: closed",
                    ));
                }
            }
        }
    }
    let reason = result?;
    Ok(acp::PromptResponse::new(reason))
}

async fn drive(
    runtime: &RuntimeHandle,
    events: &mut broadcast::Receiver<kraai_runtime::RuntimeEvent>,
    output: &mut Output,
    cancelled: &CancellationToken,
) -> Result<acp::StopReason> {
    loop {
        let received = tokio::select! {
            biased;
            () = cancelled.cancelled() => return Ok(acp::StopReason::Cancelled),
            received = events.recv() => received,
        };
        let event = match received {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                if let Some(reason) = output.catch_up(runtime, cancelled).await? {
                    return Ok(reason);
                }
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                return Err(crate::error::internal(
                    "Runtime event stream interrupted: closed",
                ));
            }
        };
        if let Event::ServiceError { error } = event.event {
            return Err(crate::error::runtime(error));
        }
        if !output.uncovered(&event) {
            continue;
        }
        match event.event {
            Event::TurnCompleted { .. } => return Ok(acp::StopReason::EndTurn),
            Event::StreamError { error, .. } | Event::ContinuationFailed { error, .. } => {
                return Err(crate::error::internal(error));
            }
            Event::SessionError { error, .. } => return Err(crate::error::runtime(error)),
            event @ Event::StreamCancelled { .. } => {
                output.event(runtime, event).await?;
                return Ok(acp::StopReason::Cancelled);
            }
            Event::HistoryUpdated { .. } => {
                if let Some(reason) = output.catch_up(runtime, cancelled).await? {
                    return Ok(reason);
                }
            }
            Event::ScriptApprovalRequested { script, .. } => {
                if !output.approve(runtime, script, cancelled).await? {
                    return Ok(acp::StopReason::Cancelled);
                }
            }
            event => output.event(runtime, event).await?,
        }
    }
}

struct Output {
    id: acp::SessionId,
    session_id: SessionId,
    connection: Connection,
    executions: HashMap<String, String>,
    /// Bytes of each assistant text item already sent, by message and item index.
    sent: HashMap<(String, usize), usize>,
    /// Messages that already showed an item, so their next text starts a new paragraph.
    shown: HashSet<String>,
    /// Tool calls announced to the client, and those still waiting for a result.
    announced: HashSet<String>,
    open: HashSet<String>,
    /// The session tip before the prompt was submitted; the prompt's messages descend from it.
    prompt_start: Option<MessageId>,
    /// The global event sequence the last resync already covered.
    covered: u64,
    /// The message streamed most recently, which runtime notices are shown in.
    streaming: Option<String>,
}

impl Output {
    fn new(session_id: SessionId, connection: Connection, prompt_start: Option<MessageId>) -> Self {
        Self {
            id: acp::SessionId::new(session_id.to_string()),
            session_id,
            connection,
            executions: HashMap::new(),
            sent: HashMap::new(),
            shown: HashSet::new(),
            announced: HashSet::new(),
            open: HashSet::new(),
            prompt_start,
            covered: 0,
            streaming: None,
        }
    }

    /// Whether `event` belongs to this session and is newer than the last resync.
    fn uncovered(&self, event: &kraai_runtime::RuntimeEvent) -> bool {
        event.sequence > self.covered && event.event.session_id() == Some(&self.session_id)
    }

    /// This prompt's messages in the snapshot, oldest first.
    fn prompt_messages<'a>(
        &self,
        snapshot: &'a kraai_runtime::SessionSnapshot,
    ) -> Vec<&'a kraai_types::Message> {
        let mut messages = Vec::new();
        let mut cursor = snapshot.session.tip_id.clone().map(MessageId::new);
        while let Some(id) = cursor
            && Some(&id) != self.prompt_start.as_ref()
            && let Some(message) = snapshot.history.get(&id)
        {
            messages.push(message);
            cursor = message.parent_id.clone();
        }
        messages.reverse();
        messages
    }

    fn update(&self, update: acp::SessionUpdate) -> Result<()> {
        self.connection
            .send_notification(acp::SessionNotification::new(self.id.clone(), update))
    }

    fn text(&self, message_id: &str, text: String) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.update(acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(content::text(text)).message_id(acp::MessageId::new(message_id)),
        ))
    }

    /// Sends `text` appended to the text item at `index`, starting a paragraph for a new item.
    fn item_text(&mut self, message_id: &str, index: usize, text: &str) -> Result<()> {
        let sent = self
            .sent
            .entry((message_id.to_string(), index))
            .or_insert(0);
        let paragraph = *sent == 0 && !text.is_empty() && self.shown.contains(message_id);
        *sent += text.len();
        if !text.is_empty() {
            self.shown.insert(message_id.to_string());
        }
        let text = if paragraph {
            format!("\n\n{text}")
        } else {
            text.to_string()
        };
        self.text(message_id, text)
    }

    /// Shows a runtime notice as its own paragraph, in `message_id` when it belongs to one.
    fn notice(&mut self, message_id: Option<&str>, notice: &str) -> Result<()> {
        let mut chunk = acp::ContentChunk::new(content::text(format!("\n\n[{notice}]\n\n")));
        if let Some(message_id) = message_id {
            self.shown.remove(message_id);
            chunk = chunk.message_id(acp::MessageId::new(message_id));
        }
        self.update(acp::SessionUpdate::AgentMessageChunk(chunk))
    }

    fn announce(&mut self, call_id: String, input: String) -> Result<()> {
        if !self.announced.insert(call_id.clone()) {
            return Ok(());
        }
        self.open.insert(call_id.clone());
        self.update(acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(call_id, "Run Nushell script")
                .kind(acp::ToolKind::Execute)
                .status(acp::ToolCallStatus::Pending)
                .raw_input(serde_json::Value::String(input)),
        ))
    }

    async fn result(
        &mut self,
        runtime: &RuntimeHandle,
        call_id: String,
        output: &kraai_types::MessageContent,
        outcome: kraai_execution_types::ScriptExecutionOutcome,
    ) -> Result<()> {
        if self.open.remove(&call_id) || !self.announced.contains(&call_id) {
            self.announced.insert(call_id.clone());
            self.update(content::tool_result(runtime, call_id, output, outcome).await?)?;
        }
        Ok(())
    }

    async fn event(&mut self, runtime: &RuntimeHandle, event: Event) -> Result<()> {
        match event {
            Event::ItemStarted {
                message_id,
                item_index,
                item,
                ..
            } => match item {
                AssistantItem::Text { text, .. } => {
                    self.item_text(&message_id, item_index, &text)?;
                }
                AssistantItem::ScriptCall { .. } => {
                    self.shown.insert(message_id);
                }
                AssistantItem::Reasoning { .. } => {}
            },
            Event::ItemDelta {
                message_id,
                item_index,
                delta,
                ..
            } => self.item_text(&message_id, item_index, &delta)?,
            Event::ReasoningDelta {
                message_id, delta, ..
            } => {
                self.update(acp::SessionUpdate::AgentThoughtChunk(
                    acp::ContentChunk::new(content::text(delta))
                        .message_id(acp::MessageId::new(message_id)),
                ))?;
            }
            Event::StreamStart { message_id, .. } => self.streaming = Some(message_id),
            Event::StreamRestarted { message_id, .. } => {
                self.sent.retain(|(id, _), _| id != &message_id);
                self.notice(
                    Some(&message_id),
                    "Provider stream failed; retrying the response.",
                )?;
            }
            Event::ContextStateChanged { notifications, .. } => {
                let message_id = self.streaming.clone();
                for notification in notifications {
                    self.notice(message_id.as_deref(), &notification)?;
                }
            }
            Event::ScriptPrepared {
                execution_id,
                call_id,
                source,
                ..
            } => {
                self.executions.insert(execution_id, call_id.clone());
                self.announce(call_id, source)?;
            }
            Event::ScriptStarted { call_id, .. } => {
                self.update(acp::SessionUpdate::ToolCallUpdate(
                    acp::ToolCallUpdate::new(
                        call_id,
                        acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::InProgress),
                    ),
                ))?;
            }
            Event::ScriptResultReady {
                call_id,
                output,
                outcome,
                execution_id,
                ..
            } => {
                self.executions.remove(&execution_id);
                self.result(runtime, call_id, &output, outcome).await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Sends whatever this prompt's messages gained that the client has not seen, after
    /// events were missed.
    async fn resync(&mut self, runtime: &RuntimeHandle) -> Result<kraai_runtime::SessionSnapshot> {
        let snapshot = runtime
            .get_session_snapshot(self.session_id.clone())
            .await
            .map_err(error::runtime)?;
        self.covered = self.covered.max(snapshot.event_sequence);
        for message in self.prompt_messages(&snapshot) {
            let id = message.id.to_string();
            match &message.content {
                ConversationItem::Assistant { items } => {
                    for (index, item) in items.iter().enumerate() {
                        match item {
                            AssistantItem::Text { text, .. } => {
                                let sent =
                                    self.sent.get(&(id.clone(), index)).copied().unwrap_or(0);
                                let missing = text.get(sent..).unwrap_or_default();
                                self.item_text(&id, index, missing)?;
                            }
                            AssistantItem::ScriptCall { call_id, input, .. } => {
                                self.shown.insert(id.clone());
                                self.announce(call_id.to_string(), input.clone())?;
                            }
                            AssistantItem::Reasoning { .. } => {}
                        }
                    }
                }
                ConversationItem::ScriptResult {
                    call_id,
                    output,
                    outcome,
                } => {
                    self.result(runtime, call_id.to_string(), output, *outcome)
                        .await?;
                }
                _ => {}
            }
        }
        Ok(snapshot)
    }

    /// Recovers from missed events and returns the stop reason if the turn already ended.
    async fn catch_up(
        &mut self,
        runtime: &RuntimeHandle,
        cancelled: &CancellationToken,
    ) -> Result<Option<acp::StopReason>> {
        let snapshot = self.resync(runtime).await?;
        if let Some(script) = snapshot.pending_script {
            if !self.approve(runtime, script, cancelled).await? {
                return Ok(Some(acp::StopReason::Cancelled));
            }
            return Ok(None);
        }
        Ok((!snapshot.session.is_running).then_some(acp::StopReason::EndTurn))
    }

    /// Asks the client about a pending script; returns false when the prompt was cancelled.
    async fn approve(
        &mut self,
        runtime: &RuntimeHandle,
        script: PendingScriptInfo,
        cancelled: &CancellationToken,
    ) -> Result<bool> {
        self.executions
            .insert(script.execution_id.clone(), script.call_id.clone());
        self.announce(script.call_id.clone(), script.source.clone())?;
        tokio::select! {
            biased;
            () = cancelled.cancelled() => Ok(false),
            result = self.permission(runtime, script) => {
                if !result? {
                    cancelled.cancel();
                    return Ok(false);
                }
                Ok(true)
            }
        }
    }

    async fn permission(&self, runtime: &RuntimeHandle, script: PendingScriptInfo) -> Result<bool> {
        let call_id = self
            .executions
            .get(&script.execution_id)
            .ok_or_else(|| error::internal("Approval has no corresponding tool call"))?;
        let call = acp::ToolCallUpdate::new(
            call_id.clone(),
            acp::ToolCallUpdateFields::new()
                .title(format!(
                    "Allow Nushell capabilities: {}",
                    script.capability_additions.join(", ")
                ))
                .kind(acp::ToolKind::Execute)
                .raw_input(serde_json::Value::String(script.source)),
        );
        let request = acp::RequestPermissionRequest::new(
            self.id.clone(),
            call,
            vec![
                acp::PermissionOption::new(
                    "allow",
                    "Allow once",
                    acp::PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new("deny", "Reject", acp::PermissionOptionKind::RejectOnce),
            ],
        );
        let response = self.connection.send_request(request).block_task().await?;
        match response.outcome {
            acp::RequestPermissionOutcome::Selected(selected)
                if selected.option_id.0.as_ref() == "allow" =>
            {
                runtime
                    .approve_script(self.session_id.clone(), script.execution_id)
                    .await
                    .map_err(error::runtime)?;
            }
            acp::RequestPermissionOutcome::Selected(selected)
                if selected.option_id.0.as_ref() == "deny" =>
            {
                runtime
                    .deny_script(self.session_id.clone(), script.execution_id)
                    .await
                    .map_err(error::runtime)?;
            }
            acp::RequestPermissionOutcome::Cancelled => return Ok(false),
            _ => return Err(error::invalid("Unknown permission option")),
        }
        Ok(true)
    }
}
