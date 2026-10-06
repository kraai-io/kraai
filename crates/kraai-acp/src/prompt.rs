use std::collections::HashMap;

use agent_client_protocol::{Client, ConnectionTo, Result, schema::v1 as acp};
use kraai_runtime::{ContinueSessionOutcome, Event, PendingScriptInfo, RuntimeHandle};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{commands, content, error, session::ActiveTurn};

pub(crate) async fn run(
    runtime: &RuntimeHandle,
    turn: ActiveTurn,
    request: acp::PromptRequest,
    connection: ConnectionTo<Client>,
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
    let id = request.session_id;
    let mut events = runtime.subscribe();
    let model = turn.session.model.lock().await.clone();
    match commands::parse(&request.prompt)? {
        Some(commands::Command::Undo) => {
            commands::undo(runtime, &id, &connection).await?;
            return Ok(command_response());
        }
        Some(commands::Command::Continue) => {
            if runtime
                .continue_turn(id.to_string(), model.model, model.provider)
                .await
                .map_err(error::runtime)?
                == ContinueSessionOutcome::NothingToContinue
            {
                return Ok(command_response());
            }
        }
        None => {
            let message = content::prompt(runtime, request.prompt).await?;
            if cancelled.is_cancelled() {
                return Ok(acp::PromptResponse::new(acp::StopReason::Cancelled));
            }
            runtime
                .send_content(id.to_string(), message, model.model, model.provider)
                .await
                .map_err(error::runtime)?;
        }
    }
    let mut output = Output::new(id.clone(), connection);
    let result = drive(runtime, &mut events, &mut output, cancelled).await;
    if cancelled.is_cancelled() || result.is_err() {
        runtime
            .cancel_turn(id.to_string())
            .await
            .map_err(error::runtime)?;
        while let Ok(event) = events.try_recv() {
            if event.event.session_id() == Some(id.0.as_ref()) {
                output.event(runtime, event.event).await?;
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
        let event = tokio::select! {
            biased;
            () = cancelled.cancelled() => return Ok(acp::StopReason::Cancelled),
            event = events.recv() => event.map_err(|error| crate::error::internal(format!("Runtime event stream interrupted: {error}")))?.event,
        };
        if let Event::ServiceError { error } = event {
            return Err(crate::error::runtime(error));
        }
        if event.session_id() != Some(output.id.0.as_ref()) {
            continue;
        }
        match event {
            Event::TurnCompleted { .. } => return Ok(acp::StopReason::EndTurn),
            Event::StreamError { error, .. } | Event::ContinuationFailed { error, .. } => {
                return Err(crate::error::internal(error));
            }
            Event::SessionError { error, .. } => return Err(crate::error::runtime(error)),
            event @ Event::StreamCancelled { .. } => {
                output.event(runtime, event).await?;
                return Ok(acp::StopReason::Cancelled);
            }
            Event::ScriptApprovalRequested { script, .. } => {
                tokio::select! {
                    biased;
                    () = cancelled.cancelled() => return Ok(acp::StopReason::Cancelled),
                    result = output.permission(runtime, script) => {
                        if !result? { cancelled.cancel(); }
                    }
                }
            }
            event => output.event(runtime, event).await?,
        }
    }
}

struct Output {
    id: acp::SessionId,
    connection: ConnectionTo<Client>,
    executions: HashMap<String, String>,
}

impl Output {
    fn new(id: acp::SessionId, connection: ConnectionTo<Client>) -> Self {
        Self {
            id,
            connection,
            executions: HashMap::new(),
        }
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

    async fn event(&mut self, runtime: &RuntimeHandle, event: Event) -> Result<()> {
        match event {
            Event::StreamChunk {
                message_id, chunk, ..
            } => {
                self.text(&message_id, chunk)?;
            }
            Event::ScriptPrepared {
                execution_id,
                call_id,
                source,
                ..
            } => {
                self.executions.insert(execution_id, call_id.clone());
                self.update(acp::SessionUpdate::ToolCall(
                    acp::ToolCall::new(call_id, "Run Nushell script")
                        .kind(acp::ToolKind::Execute)
                        .status(acp::ToolCallStatus::Pending)
                        .raw_input(serde_json::Value::String(source)),
                ))?;
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
                execution_id,
                ..
            } => {
                self.executions.remove(&execution_id);
                self.update(content::tool_result(runtime, call_id, &output).await?)?;
            }
            _ => {}
        }
        Ok(())
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
                    .approve_script(self.id.to_string(), script.execution_id)
                    .await
                    .map_err(error::runtime)?;
            }
            acp::RequestPermissionOutcome::Selected(selected)
                if selected.option_id.0.as_ref() == "deny" =>
            {
                runtime
                    .deny_script(self.id.to_string(), script.execution_id)
                    .await
                    .map_err(error::runtime)?;
            }
            acp::RequestPermissionOutcome::Cancelled => return Ok(false),
            _ => return Err(error::invalid("Unknown permission option")),
        }
        Ok(true)
    }
}
