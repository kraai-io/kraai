use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{
    CompactionCheckpoint, FileContextSnapshot, RequestUsageStore, SqliteCompactionStore,
};
use kraai_provider_core::{ProviderManager, ProviderRequest};
use kraai_types::{ConversationItem, Message, ModelId, ProviderId, RequestUsage};

mod summary;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct ContextCompaction {
    pub(crate) store: SqliteCompactionStore,
    pub(crate) usage_store: Arc<dyn RequestUsageStore>,
    pub(crate) session_id: String,
    pub(crate) original: ProviderRequest,
    pub(crate) prefix: String,
    pub(crate) snapshots: Vec<FileContextSnapshot>,
    pub(crate) file_notifications: Vec<String>,
    pub(crate) history: Vec<Message>,
    pub(crate) previous: Option<CompactionCheckpoint>,
    pub(crate) on_usage: Option<Arc<dyn Fn(RequestUsage) + Send + Sync>>,
    pub(crate) image_resolver: Option<Arc<dyn kraai_provider_core::ImageResolver>>,
    pub(crate) usage_barrier: Option<Arc<tokio::sync::RwLock<()>>>,
}

impl std::fmt::Debug for ContextCompaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextCompaction")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

pub struct CompactionOutcome {
    pub request: ProviderRequest,
    pub notification: String,
    pub requests: Vec<RequestUsage>,
}

pub(crate) fn assemble(
    prefix: &str,
    snapshots: &[FileContextSnapshot],
    previous: Option<&CompactionCheckpoint>,
    history: &[Message],
    tool: Option<kraai_provider_core::ScriptToolDefinition>,
) -> ProviderRequest {
    let mut messages = vec![ConversationItem::System {
        text: prefix.to_string(),
    }];
    if let Some(checkpoint) = previous {
        messages.extend(checkpoint.replacement.clone());
    }
    let retained: HashSet<_> = history.iter().map(|message| &message.id).collect();
    let mut anchored: HashMap<_, Vec<_>> = HashMap::new();
    for snapshot in snapshots {
        let item = ConversationItem::FileContext {
            text: snapshot.text.clone(),
        };
        if retained.contains(&snapshot.anchor) {
            anchored.entry(&snapshot.anchor).or_default().push(item);
        } else {
            messages.push(item);
        }
    }
    for message in history {
        messages.push(message.content.clone());
        if let Some(files) = anchored.remove(&message.id) {
            messages.extend(files);
        }
    }
    let mut request = ProviderRequest {
        messages,
        script_tool: tool,
        cacheable_messages: None,
    };
    limit_request_images(&mut request);
    request
}

impl ContextCompaction {
    pub fn with_image_resolver(
        mut self,
        resolver: Arc<dyn kraai_provider_core::ImageResolver>,
    ) -> Self {
        self.image_resolver = Some(resolver);
        self
    }

    pub fn observe_usage(
        mut self,
        barrier: Arc<tokio::sync::RwLock<()>>,
        observer: Arc<dyn Fn(RequestUsage) + Send + Sync>,
    ) -> Self {
        self.on_usage = Some(observer);
        self.usage_barrier = Some(barrier);
        self
    }

    pub async fn run(
        &self,
        providers: &ProviderManager,
        provider_id: &ProviderId,
        model_id: &ModelId,
    ) -> Result<CompactionOutcome> {
        let covered = self
            .history
            .last()
            .ok_or_else(|| eyre!("Missing compaction boundary"))?;
        let native = providers.supports_native_compaction(provider_id, model_id)?;
        let mut requests = Vec::new();
        let summary = self
            .summarize(providers, provider_id, model_id, native, &mut requests)
            .await?;
        let mut replacement = retained_users(
            &self.original.messages,
            if native { 64_000 } else { 20_000 },
        );
        replacement.push(summary);
        let checkpoint = CompactionCheckpoint {
            covered_through: covered.id.clone(),
            superseded_usage: Vec::new(),
            previous_boundary: self
                .previous
                .as_ref()
                .map(|checkpoint| checkpoint.covered_through.clone()),
            replacement,
            provider_id: provider_id.clone(),
            model_id: model_id.clone(),
            prompt_version: 1,
            usage: requests.last().and_then(|request| request.usage.clone()),
        };
        if let Some(barrier) = &self.usage_barrier {
            self.store
                .save_with_barrier(&checkpoint, barrier.clone())
                .await?;
        } else {
            self.store.save(&checkpoint).await?;
        }
        let mut request = assemble(
            &self.prefix,
            &self.snapshots,
            Some(&checkpoint),
            &[],
            self.original.script_tool.clone(),
        );
        crate::context_state::append_notifications(&mut request, &self.file_notifications);
        Ok(CompactionOutcome {
            request,
            notification: "Conversation context compacted.".into(),
            requests,
        })
    }
}

fn retained_users(messages: &[ConversationItem], token_budget: usize) -> Vec<ConversationItem> {
    let mut remaining = token_budget.saturating_mul(4);
    let mut retained = Vec::new();
    for item in messages.iter().rev() {
        let ConversationItem::User { content } = item else {
            continue;
        };
        if remaining == 0 {
            break;
        }
        let content = content.without_images();
        let text = content.display_text();
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 && !text.is_empty() {
            break;
        }
        retained.push(ConversationItem::User {
            content: text.get(..end).unwrap_or_default().into(),
        });
        remaining = remaining.saturating_sub(end);
    }
    retained.reverse();
    retained
}

pub(crate) fn limit_request_images(request: &mut ProviderRequest) {
    let mut count = 0usize;
    let mut bytes = 0u64;
    for message in request.messages.iter_mut().rev() {
        let content = match message {
            ConversationItem::User { content } => content,
            ConversationItem::ScriptResult { output, .. } => output,
            _ => continue,
        };
        for part in content.0.iter_mut().rev() {
            if let kraai_types::ContentPart::Image { image } = part {
                if count < kraai_types::image::MAX_REQUEST_IMAGES
                    && bytes.saturating_add(image.byte_length)
                        <= kraai_types::image::MAX_REQUEST_IMAGE_BYTES
                {
                    count += 1;
                    bytes += image.byte_length;
                } else {
                    *part = kraai_types::ContentPart::omitted_image(image);
                }
            }
        }
    }
}
