use kraai_persistence::{FileImageStore, ScriptExecutionStore};
use kraai_types::{ImageAttachment, ScriptExecutionId};
use std::sync::Arc;

pub(super) struct StoredImageResolver(pub(super) FileImageStore);

#[async_trait::async_trait]
impl kraai_provider_core::ImageResolver for StoredImageResolver {
    async fn resolve(&self, image: &ImageAttachment) -> color_eyre::Result<Vec<u8>> {
        self.0.read(image).await
    }
}

pub(super) struct DurableImageAttachments {
    pub(super) execution_id: ScriptExecutionId,
    pub(super) session_id: String,
    pub(super) agent: Arc<tokio::sync::RwLock<kraai_agent::AgentManager>>,
    pub(super) images: Arc<FileImageStore>,
    pub(super) executions: Arc<dyn ScriptExecutionStore>,
}

impl kraai_nushell_runtime::ImageAttachmentHandler for DurableImageAttachments {
    fn attach<'a>(
        &'a self,
        sequence: u64,
        bytes: Vec<u8>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ImageAttachment, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let record = self
                .executions
                .get(&self.execution_id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "Image execution is missing".to_string())?;
            if sequence == 0 || record.phase != kraai_types::ScriptExecutionPhase::Running {
                return Err("Image execution is not running or sequence is invalid".into());
            }
            if !record.images.contains_key(&sequence) {
                if record.images.len() >= kraai_types::image::MAX_IMAGE_ATTACHMENTS {
                    return Err("Too many image attachments in one script execution".into());
                }
                let total = record
                    .images
                    .values()
                    .map(|image| image.byte_length)
                    .try_fold(bytes.len() as u64, u64::checked_add);
                if total.is_none_or(|total| total > kraai_types::image::MAX_REQUEST_IMAGE_BYTES) {
                    return Err("Image attachments exceed the execution byte limit".into());
                }
            }
            let image = self
                .images
                .import(bytes)
                .await
                .map_err(|error| error.to_string())?;
            self.executions
                .append_image(&self.execution_id, sequence, image.clone())
                .await
                .map_err(|error| error.to_string())?;
            Ok(image)
        })
    }
    fn attach_existing<'a>(
        &'a self,
        sequence: u64,
        id: String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ImageAttachment, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let history = self
                .agent
                .read()
                .await
                .get_chat_history(&self.session_id)
                .await
                .map_err(|error| error.to_string())?;
            let image = history
                .values()
                .find_map(|message| {
                    let content = match &message.content {
                        kraai_types::ConversationItem::User { content } => content,
                        kraai_types::ConversationItem::ScriptResult { output, .. } => output,
                        _ => return None,
                    };
                    content.images().find(|image| image.id == id).cloned()
                })
                .ok_or_else(|| "Image attachment is not in this session's history".to_string())?;
            self.images
                .read(&image)
                .await
                .map_err(|error| error.to_string())?;
            self.executions
                .append_image(&self.execution_id, sequence, image.clone())
                .await
                .map_err(|error| error.to_string())?;
            Ok(image)
        })
    }
}
