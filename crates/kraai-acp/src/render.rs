use std::collections::{HashMap, HashSet};

use agent_client_protocol::{Result, v1::schema as acp};
use kraai_runtime::{Event, RuntimeEvent, RuntimeHandle, SessionSnapshot};
use kraai_types::{AssistantItem, ConversationItem, Message, MessageId, MessageStatus, ScriptCall};

use crate::content;
use crate::transport::Connection;

pub(crate) struct Renderer {
    pub(crate) id: acp::SessionId,
    pub(crate) session_id: kraai_types::SessionId,
    pub(crate) connection: Connection,
    sent: HashMap<(String, usize), usize>,
    shown: HashSet<String>,
    announced: HashSet<String>,
    open: HashSet<String>,
    settled: HashSet<MessageId>,
    covered: u64,
    retry: Option<String>,
}

impl Renderer {
    pub(crate) fn new(
        session_id: kraai_types::SessionId,
        connection: Connection,
        covered: u64,
    ) -> Self {
        Self {
            id: acp::SessionId::new(session_id.to_string()),
            session_id,
            connection,
            sent: HashMap::new(),
            shown: HashSet::new(),
            announced: HashSet::new(),
            open: HashSet::new(),
            settled: HashSet::new(),
            covered,
            retry: None,
        }
    }

    pub(crate) fn accepts(&self, event: &RuntimeEvent) -> bool {
        event.sequence > self.covered && event.event.session_id() == Some(&self.session_id)
    }

    pub(crate) fn absorb(&mut self, message: &Message) {
        let id = message.id.to_string();
        match &message.content {
            ConversationItem::Assistant { items } => {
                for (index, item) in items.iter().enumerate() {
                    match item {
                        AssistantItem::Text { text, .. } => {
                            self.sent.insert((id.clone(), index), text.len());
                            if !text.is_empty() {
                                self.shown.insert(id.clone());
                            }
                        }
                        AssistantItem::ScriptCall(ScriptCall { call_id, .. }) => {
                            self.shown.insert(id.clone());
                            self.announced.insert(call_id.to_string());
                            self.open.insert(call_id.to_string());
                        }
                        AssistantItem::Reasoning { .. } => {}
                    }
                }
            }
            ConversationItem::ScriptResult { call_id, .. } => {
                self.announced.insert(call_id.to_string());
                self.open.remove(call_id.as_str());
            }
            ConversationItem::FileContext { .. }
            | ConversationItem::Compaction { .. }
            | ConversationItem::System { .. }
            | ConversationItem::User { .. } => {}
        }
        self.settle(message);
    }

    fn settle(&mut self, message: &Message) {
        if !matches!(message.status, MessageStatus::Streaming { .. }) {
            self.settled.insert(message.id.clone());
        }
    }

    pub(crate) fn skip_user(&mut self, message: &Message) {
        self.settled.insert(message.id.clone());
    }

    pub(crate) async fn user(&mut self, runtime: &RuntimeHandle, message: &Message) -> Result<()> {
        let ConversationItem::User { content } = &message.content else {
            return Ok(());
        };
        if !self.settled.insert(message.id.clone()) {
            return Ok(());
        }
        for block in content::blocks(runtime, content).await? {
            self.update(acp::SessionUpdate::UserMessageChunk(
                acp::ContentChunk::new(block)
                    .message_id(acp::MessageId::new(message.id.to_string())),
            ))?;
        }
        Ok(())
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

    pub(crate) fn settle_retry(&mut self, status: acp::ToolCallStatus) -> Result<()> {
        let Some(id) = self.retry.take() else {
            return Ok(());
        };
        self.update(acp::SessionUpdate::ToolCallUpdate(
            acp::ToolCallUpdate::new(id, acp::ToolCallUpdateFields::new().status(status)),
        ))
    }

    pub(crate) fn announce(&mut self, call_id: String, input: String) -> Result<()> {
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

    pub(crate) async fn event(&mut self, runtime: &RuntimeHandle, event: Event) -> Result<()> {
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
                call_id, source, ..
            } => {
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
                ..
            } => {
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

    pub(crate) async fn sync(
        &mut self,
        runtime: &RuntimeHandle,
        snapshot: &SessionSnapshot,
        own_user: Option<&MessageId>,
    ) -> Result<()> {
        self.covered = self.covered.max(snapshot.event_sequence);
        let mut messages = Vec::new();
        let mut cursor = snapshot.session.tip_id.clone().map(MessageId::new);
        while let Some(id) = cursor
            && !self.settled.contains(&id)
            && let Some(message) = snapshot.history.get(&id)
        {
            messages.push(message);
            cursor = message.parent_id.clone();
        }
        for message in messages.into_iter().rev() {
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
                ConversationItem::User { .. } if own_user == Some(&message.id) => {}
                ConversationItem::User { .. } => {
                    self.user(runtime, message).await?;
                }
                ConversationItem::FileContext { .. }
                | ConversationItem::Compaction { .. }
                | ConversationItem::System { .. } => {}
            }
            self.settle(message);
        }
        Ok(())
    }
}
