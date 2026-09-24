use std::hash::{DefaultHasher, Hash, Hasher};

use color_eyre::Result;
use kraai_types::ConversationItem;

use crate::ScriptToolDefinition;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Prefix {
    messages: Vec<u64>,
    tool: Option<ScriptToolDefinition>,
    pub(super) bytes: usize,
}

impl Prefix {
    pub(super) fn new(
        source: &[ConversationItem],
        tool: &Option<ScriptToolDefinition>,
    ) -> Result<Self> {
        let (messages, bytes) = fingerprints(source)?;
        Ok(Self {
            messages,
            tool: tool.clone(),
            bytes,
        })
    }

    pub(super) fn extends(&self, previous: &Self) -> bool {
        self.tool == previous.tool && self.messages.starts_with(&previous.messages)
    }
}

pub(super) fn fingerprints(source: &[ConversationItem]) -> Result<(Vec<u64>, usize)> {
    let mut bytes = 0_usize;
    let mut messages = Vec::with_capacity(source.len());
    for message in source {
        let encoded = serde_json::to_vec(message)?;
        bytes = bytes.saturating_add(encoded.len());
        let mut hash = DefaultHasher::new();
        encoded.hash(&mut hash);
        messages.push(hash.finish());
    }
    Ok((messages, bytes))
}
