use std::collections::{BTreeMap, HashMap, HashSet};

use color_eyre::eyre::{Report, Result, eyre};
use kraai_types::{Message, MessageId};
use rusqlite::Connection;

use crate::MessageStore;
use crate::stream_deltas::read_message;

pub(crate) fn missing_history_message(id: &MessageId) -> Report {
    eyre!("Message {id} disappeared while reading session history")
}

pub(crate) fn history_cycle(id: &MessageId) -> Report {
    eyre!("Corrupt message parent graph: cycle repeats message {id}")
}

pub(crate) fn read_history_chain(
    connection: &Connection,
    from: &MessageId,
    limit: usize,
) -> Result<Vec<Message>> {
    let mut history = Vec::new();
    let mut visited = HashSet::new();
    let mut cursor = Some(from.clone());
    while let Some(id) = cursor
        && history.len() < limit
    {
        if !visited.insert(id.clone()) {
            return Err(history_cycle(&id));
        }
        let message =
            read_message(connection, id.as_str())?.ok_or_else(|| missing_history_message(&id))?;
        cursor = message.parent_id.clone();
        history.push(message);
    }
    Ok(history)
}

/// Walks a message parent chain from a tip toward the root in pages that
/// double in size, so callers that stop early read little and full walks
/// take a logarithmic number of store round trips.
pub struct HistoryCursor<'a> {
    store: &'a dyn MessageStore,
    overlay: Option<&'a HashMap<MessageId, Message>>,
    next: Option<MessageId>,
    visited: HashSet<MessageId>,
    page_size: usize,
}

impl<'a> HistoryCursor<'a> {
    pub fn new(store: &'a dyn MessageStore, from: Option<MessageId>) -> Self {
        Self {
            store,
            overlay: None,
            next: from,
            visited: HashSet::new(),
            page_size: 1,
        }
    }

    /// Prefers these in-memory versions over persisted messages with the same id.
    pub fn with_overlay(mut self, overlay: &'a HashMap<MessageId, Message>) -> Self {
        self.overlay = Some(overlay);
        self
    }

    pub async fn next_page(&mut self) -> Result<Option<Vec<Message>>> {
        let Some(mut cursor) = self.next.take() else {
            return Ok(None);
        };
        let overlay = self.overlay;
        let mut page = Vec::new();
        while let Some(message) = overlay.and_then(|overlay| overlay.get(&cursor)) {
            self.push(&mut page, cursor, message.clone())?;
            match message.parent_id.clone() {
                Some(parent) if page.len() < self.page_size => cursor = parent,
                _ => return Ok(Some(self.finish_page(page))),
            }
        }
        let mut requested = Some(cursor.clone());
        for message in self
            .store
            .read_history(&cursor, self.page_size - page.len())
            .await?
        {
            let id = requested
                .take()
                .ok_or_else(|| eyre!("History page continued past the root"))?;
            requested = message.parent_id.clone();
            let message = overlay
                .and_then(|overlay| overlay.get(&id))
                .cloned()
                .unwrap_or(message);
            self.push(&mut page, id, message)?;
        }
        Ok(Some(self.finish_page(page)))
    }

    /// Reads up to `limit` messages, tip first, and returns where the next older page starts.
    pub async fn read_page(mut self, limit: usize) -> Result<(Vec<Message>, Option<MessageId>)> {
        self.page_size = limit.max(1);
        let page = self.next_page().await?.unwrap_or_default();
        Ok((page, self.next))
    }

    pub async fn read_to_root(mut self) -> Result<Vec<Message>> {
        self.page_size = usize::MAX;
        let mut history = Vec::new();
        while let Some(page) = self.next_page().await? {
            history.extend(page);
        }
        Ok(history)
    }

    fn push(&mut self, page: &mut Vec<Message>, id: MessageId, message: Message) -> Result<()> {
        if !self.visited.insert(id.clone()) {
            return Err(history_cycle(&id));
        }
        page.push(message);
        Ok(())
    }

    fn finish_page(&mut self, page: Vec<Message>) -> Vec<Message> {
        self.next = page.last().and_then(|message| message.parent_id.clone());
        self.page_size = self.page_size.saturating_mul(2);
        page
    }
}

/// Indexes a tip-first history by message id, keeping the occurrence nearest the tip.
pub fn history_by_id(history: Vec<Message>) -> BTreeMap<MessageId, Message> {
    let mut indexed = BTreeMap::new();
    for message in history {
        indexed.entry(message.id.clone()).or_insert(message);
    }
    indexed
}
