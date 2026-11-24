use std::collections::{HashMap, HashSet};

use agent_client_protocol::{Result, v1::schema as acp};
use kraai_runtime::{Event, PendingScriptInfo, RuntimeHandle, SubmitMessageOutcome, TurnOutcome};
use kraai_types::{AssistantItem, ConversationItem, MessageId, ScriptCall, SessionId};
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
    if commands::is_undo(&request.prompt)? {
        commands::undo(runtime, &id, &connection).await?;
        return Ok(command_response());
    }
    let message = content::prompt(runtime, request.prompt).await?;
    let Some(idle) = wait_until_idle(runtime, &id, &mut events, cancelled).await? else {
        return Ok(acp::PromptResponse::new(acp::StopReason::Cancelled));
    };
    if cancelled.is_cancelled() {
        return Ok(acp::PromptResponse::new(acp::StopReason::Cancelled));
    }
    let outcome = runtime
        .send_content(
            id.clone(),
            message,
            model.model,
            model.provider,
            model.options,
        )
        .await
        .map_err(error::runtime)?;
    let stream = match outcome {
        SubmitMessageOutcome::Started { message_id } => message_id,
        SubmitMessageOutcome::Queued { .. } => {
            return Err(error::invalid(
                "Another turn started in this session first, so the prompt was queued behind it",
            ));
        }
    };
    let prompt_start = idle.session.tip_id.map(MessageId::new);
    finish_prompt(
        runtime,
        &mut events,
        Output::new(id, connection, prompt_start, stream),
        cancelled,
    )
    .await
}

async fn wait_until_idle(
    runtime: &RuntimeHandle,
    id: &SessionId,
    events: &mut broadcast::Receiver<kraai_runtime::RuntimeEvent>,
    cancelled: &CancellationToken,
) -> Result<Option<kraai_runtime::SessionSnapshot>> {
    loop {
        let snapshot = runtime
            .get_session_snapshot(id.clone())
            .await
            .map_err(error::runtime)?;
        if !snapshot.session.is_running && snapshot.queued_messages == 0 {
            return Ok(Some(snapshot));
        }
        loop {
            let received = tokio::select! {
                biased;
                () = cancelled.cancelled() => return Ok(None),
                received = events.recv() => received,
            };
            let event = match received {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => break,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(error::internal("Runtime event stream interrupted: closed"));
                }
            };
            if let Event::ServiceError { error } = event.event {
                return Err(error::runtime(error));
            }
            if event.sequence > snapshot.event_sequence
                && event.event.session_id() == Some(id)
                && matches!(
                    event.event,
                    Event::SessionStateChanged {
                        is_running: false,
                        ..
                    } | Event::HistoryUpdated { .. }
                )
            {
                break;
            }
        }
    }
}

async fn finish_prompt(
    runtime: &RuntimeHandle,
    events: &mut broadcast::Receiver<kraai_runtime::RuntimeEvent>,
    mut output: Output,
    cancelled: &CancellationToken,
) -> Result<acp::PromptResponse> {
    let result = drive(runtime, events, &mut output, cancelled).await;
    if !output.ended && (cancelled.is_cancelled() || result.is_err()) {
        flush(runtime, events, &mut output).await?;
        if !output.ended {
            runtime
                .cancel_stream(output.session_id.clone())
                .await
                .map_err(error::runtime)?;
            output.cancel_sent = true;
            flush(runtime, events, &mut output).await?;
        }
    }
    output.settle_retry(match result {
        Ok(acp::StopReason::EndTurn) => acp::ToolCallStatus::Completed,
        _ => acp::ToolCallStatus::Failed,
    })?;
    let reason = result?;
    Ok(acp::PromptResponse::new(reason))
}

async fn flush(
    runtime: &RuntimeHandle,
    events: &mut broadcast::Receiver<kraai_runtime::RuntimeEvent>,
    output: &mut Output,
) -> Result<()> {
    while !output.ended {
        match events.try_recv() {
            Ok(event) => {
                if !output.accepts(&event) {
                    continue;
                }
                match event.event {
                    Event::TurnCompleted { .. }
                    | Event::StreamCancelled { .. }
                    | Event::StreamError { .. }
                    | Event::ContinuationFailed { .. } => output.ended = true,
                    Event::StreamStart { message_id, .. }
                        if output.cancel_sent && message_id != output.stream =>
                    {
                        output.ended = true;
                    }
                    event @ (Event::ScriptPrepared { .. }
                    | Event::ScriptStarted { .. }
                    | Event::ScriptOutputTail { .. }
                    | Event::RequestUsageUpdated { .. }
                    | Event::TurnTimingChanged { .. }
                    | Event::ConfigLoaded
                    | Event::ModelsUpdated
                    | Event::ServiceError { .. }
                    | Event::SessionError { .. }
                    | Event::StreamStart { .. }
                    | Event::ItemStarted { .. }
                    | Event::ItemDelta { .. }
                    | Event::ReasoningDelta { .. }
                    | Event::StreamComplete { .. }
                    | Event::ProviderRetryScheduled { .. }
                    | Event::StreamRestarted { .. }
                    | Event::ScriptApprovalRequested { .. }
                    | Event::ScriptResultReady { .. }
                    | Event::ContextStateChanged { .. }
                    | Event::SessionStateChanged { .. }
                    | Event::MessageUpserted { .. }
                    | Event::MessagesRemoved { .. }
                    | Event::HistoryUpdated { .. }
                    | Event::ProviderAuthUpdated { .. }
                    | Event::McpAuthUpdated { .. }) => output.event(runtime, event).await?,
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                output.resync(runtime).await?;
                break;
            }
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Closed) => {
                return Err(error::internal("Runtime event stream interrupted: closed"));
            }
        }
    }
    Ok(())
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
                return Err(error::internal("Runtime event stream interrupted: closed"));
            }
        };
        if let Event::ServiceError { error } = event.event {
            return Err(error::runtime(error));
        }
        if !output.accepts(&event) {
            continue;
        }
        match event.event {
            Event::TurnCompleted { .. } => {
                output.ended = true;
                return Ok(acp::StopReason::EndTurn);
            }
            Event::StreamError { error, .. } | Event::ContinuationFailed { error, .. } => {
                output.ended = true;
                return Err(error::internal(error));
            }
            Event::SessionError { error, .. } => return Err(error::runtime(error)),
            Event::StreamCancelled { .. } => {
                output.ended = true;
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
            event @ (Event::ScriptPrepared { .. }
            | Event::ScriptStarted { .. }
            | Event::ScriptOutputTail { .. }
            | Event::RequestUsageUpdated { .. }
            | Event::TurnTimingChanged { .. }
            | Event::ConfigLoaded
            | Event::ModelsUpdated
            | Event::ServiceError { .. }
            | Event::StreamStart { .. }
            | Event::ItemStarted { .. }
            | Event::ItemDelta { .. }
            | Event::ReasoningDelta { .. }
            | Event::StreamComplete { .. }
            | Event::ProviderRetryScheduled { .. }
            | Event::StreamRestarted { .. }
            | Event::ScriptResultReady { .. }
            | Event::ContextStateChanged { .. }
            | Event::SessionStateChanged { .. }
            | Event::MessageUpserted { .. }
            | Event::MessagesRemoved { .. }
            | Event::ProviderAuthUpdated { .. }
            | Event::McpAuthUpdated { .. }) => output.event(runtime, event).await?,
        }
    }
}

struct Output {
    id: acp::SessionId,
    session_id: SessionId,
    connection: Connection,
    executions: HashMap<String, String>,
    sent: HashMap<(String, usize), usize>,
    shown: HashSet<String>,
    announced: HashSet<String>,
    open: HashSet<String>,
    prompt_start: Option<MessageId>,
    stream: String,
    started: bool,
    ended: bool,
    cancel_sent: bool,
    covered: u64,
    retry: Option<String>,
}

impl Output {
    fn new(
        session_id: SessionId,
        connection: Connection,
        prompt_start: Option<MessageId>,
        stream: String,
    ) -> Self {
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
            stream,
            started: false,
            ended: false,
            cancel_sent: false,
            covered: 0,
            retry: None,
        }
    }

    fn accepts(&mut self, event: &kraai_runtime::RuntimeEvent) -> bool {
        if event.sequence <= self.covered || event.event.session_id() != Some(&self.session_id) {
            return false;
        }
        if !self.started {
            self.started = matches!(
                &event.event,
                Event::StreamStart { message_id, .. } if *message_id == self.stream
            );
        }
        self.started
    }

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

    fn update(&mut self, update: acp::SessionUpdate) -> Result<()> {
        self.connection
            .send_notification(acp::SessionNotification::new(self.id.clone(), update))
    }

    fn text(&mut self, message_id: &str, text: String) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.settle_retry(acp::ToolCallStatus::Completed)?;
        self.update(acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(content::text(text)).message_id(acp::MessageId::new(message_id)),
        ))
    }

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

    fn notice(&mut self, notice: String) -> Result<()> {
        self.settle_retry(acp::ToolCallStatus::Completed)?;
        self.update(acp::SessionUpdate::ToolCall(content::notice(notice)))
    }

    fn retry(&mut self, title: String) -> Result<()> {
        match &self.retry {
            Some(id) => self.update(acp::SessionUpdate::ToolCallUpdate(
                acp::ToolCallUpdate::new(id.clone(), acp::ToolCallUpdateFields::new().title(title)),
            )),
            None => {
                let call = content::retry(title);
                self.retry = Some(call.tool_call_id.to_string());
                self.update(acp::SessionUpdate::ToolCall(call))
            }
        }
    }

    fn settle_retry(&mut self, status: acp::ToolCallStatus) -> Result<()> {
        let Some(id) = self.retry.take() else {
            return Ok(());
        };
        self.update(acp::SessionUpdate::ToolCallUpdate(
            acp::ToolCallUpdate::new(id, acp::ToolCallUpdateFields::new().status(status)),
        ))
    }

    fn announce(&mut self, call_id: String, input: String) -> Result<()> {
        if !self.announced.insert(call_id.clone()) {
            return Ok(());
        }
        self.settle_retry(acp::ToolCallStatus::Completed)?;
        self.open.insert(call_id.clone());
        self.update(acp::SessionUpdate::ToolCall(content::script_call(
            call_id, input,
        )))
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
                AssistantItem::ScriptCall(_) => {
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
                self.settle_retry(acp::ToolCallStatus::Completed)?;
                self.update(acp::SessionUpdate::AgentThoughtChunk(
                    acp::ContentChunk::new(content::text(delta))
                        .message_id(acp::MessageId::new(message_id)),
                ))?;
            }
            Event::StreamRestarted { message_id, .. } => {
                self.sent.retain(|(id, _), _| id != &message_id);
                self.notice(String::from(
                    "Provider stream failed; retrying the response.",
                ))?;
            }
            Event::ContextStateChanged { notifications, .. } => {
                for notification in notifications {
                    self.notice(notification)?;
                }
            }
            Event::ProviderRetryScheduled {
                retry_number,
                delay_seconds,
                reason,
                ..
            } => self.retry(format!(
                "{reason}, retrying in {delay_seconds} s (retry {retry_number})"
            ))?,
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
            Event::ScriptOutputTail { call_id, lines, .. } => {
                if self.open.contains(&call_id) {
                    let text = lines
                        .iter()
                        .map(|line| line.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.update(acp::SessionUpdate::ToolCallUpdate(
                        acp::ToolCallUpdate::new(
                            call_id,
                            acp::ToolCallUpdateFields::new().content(vec![
                                acp::ToolCallContent::Content(acp::Content::new(content::text(
                                    text,
                                ))),
                            ]),
                        ),
                    ))?;
                }
            }
            Event::TurnCompleted { .. }
            | Event::RequestUsageUpdated { .. }
            | Event::TurnTimingChanged { .. }
            | Event::ConfigLoaded
            | Event::ModelsUpdated
            | Event::ServiceError { .. }
            | Event::SessionError { .. }
            | Event::StreamStart { .. }
            | Event::StreamComplete { .. }
            | Event::StreamError { .. }
            | Event::StreamCancelled { .. }
            | Event::ScriptApprovalRequested { .. }
            | Event::ContinuationFailed { .. }
            | Event::SessionStateChanged { .. }
            | Event::MessageUpserted { .. }
            | Event::MessagesRemoved { .. }
            | Event::HistoryUpdated { .. }
            | Event::ProviderAuthUpdated { .. }
            | Event::McpAuthUpdated { .. } => {}
        }
        Ok(())
    }

    async fn resync(&mut self, runtime: &RuntimeHandle) -> Result<kraai_runtime::SessionSnapshot> {
        let snapshot = runtime
            .get_session_snapshot(self.session_id.clone())
            .await
            .map_err(error::runtime)?;
        self.covered = self.covered.max(snapshot.event_sequence);
        self.started = true;
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
                            AssistantItem::ScriptCall(ScriptCall { call_id, input, .. }) => {
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
                ConversationItem::FileContext { .. }
                | ConversationItem::Compaction { .. }
                | ConversationItem::System { .. }
                | ConversationItem::User { .. } => {}
            }
        }
        Ok(snapshot)
    }

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
        if snapshot.session.is_running {
            return Ok(None);
        }
        self.ended = true;
        match snapshot.turn_outcome {
            Some(TurnOutcome::Failed { error }) => Err(error::runtime(error)),
            Some(TurnOutcome::Cancelled) => Ok(Some(acp::StopReason::Cancelled)),
            Some(TurnOutcome::Completed) | None => Ok(Some(acp::StopReason::EndTurn)),
        }
    }

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
                .raw_input(content::script_input(script.source)),
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
            acp::RequestPermissionOutcome::Selected(_) | _ => {
                return Err(error::invalid("Unknown permission option"));
            }
        }
        Ok(true)
    }
}
