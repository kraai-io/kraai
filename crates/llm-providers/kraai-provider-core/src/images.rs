use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::STANDARD};
use color_eyre::eyre::{Result, ensure, eyre};
use kraai_types::image::{MAX_REQUEST_IMAGE_BYTES, MAX_REQUEST_IMAGES};
use kraai_types::{ContentPart, ConversationItem, ImageAttachment, ModelId};

use crate::ProviderRequestContext;

#[async_trait::async_trait]
pub trait ImageResolver: Send + Sync {
    async fn resolve(&self, image: &ImageAttachment) -> Result<Vec<u8>>;
}

fn attachments(messages: &[ConversationItem]) -> impl Iterator<Item = &ImageAttachment> {
    messages.iter().flat_map(|message| {
        let parts = match message {
            ConversationItem::User { content } => content.parts(),
            ConversationItem::ScriptResult { output, .. } => output.parts(),
            _ => &[],
        };
        parts.iter().filter_map(|part| match part {
            ContentPart::Image { image } => Some(image),
            ContentPart::Text { .. } => None,
        })
    })
}

pub fn validate_image_support(
    messages: &[ConversationItem],
    model: &ModelId,
    supports_images: bool,
) -> Result<()> {
    ensure!(
        supports_images || attachments(messages).next().is_none(),
        "Model '{model}' does not support image input; select an image-capable model or configure supports_images"
    );
    Ok(())
}

#[derive(Default)]
pub struct ResolvedImages(BTreeMap<String, (ImageAttachment, String)>);

impl ResolvedImages {
    pub async fn resolve(
        messages: &[ConversationItem],
        context: &ProviderRequestContext,
    ) -> Result<Self> {
        let mut images = Self::default();
        let mut total = 0u64;
        for (index, image) in attachments(messages).enumerate() {
            image.validate().map_err(|error| eyre!(error))?;
            ensure!(
                index < MAX_REQUEST_IMAGES,
                "Too many images in model request"
            );
            total = total
                .checked_add(image.byte_length)
                .ok_or_else(|| eyre!("Image request size overflow"))?;
            ensure!(
                total <= MAX_REQUEST_IMAGE_BYTES,
                "Images exceed the 32 MiB request limit"
            );
            if let Some((existing, _)) = images.0.get(&image.id) {
                ensure!(
                    existing == image,
                    "Conflicting metadata for image attachment {}",
                    image.id
                );
                continue;
            }
            images
                .0
                .insert(image.id.clone(), (image.clone(), String::new()));
        }
        for (image, url) in images.0.values_mut() {
            let resolver = context
                .image_resolver()
                .ok_or_else(|| eyre!("Image attachments require an image resolver"))?;
            let bytes = resolver.resolve(image).await?;
            ensure!(
                bytes.len() as u64 == image.byte_length,
                "Image attachment size does not match metadata"
            );
            *url = format!("data:{};base64,{}", image.mime_type, STANDARD.encode(bytes));
        }
        Ok(images)
    }

    pub fn data_url(&self, image: &ImageAttachment) -> Result<&str> {
        let (existing, url) = self
            .0
            .get(&image.id)
            .ok_or_else(|| eyre!("Image attachment {} was not resolved", image.id))?;
        ensure!(existing == image, "Conflicting image attachment metadata");
        Ok(url)
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
