#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert image resolution behavior after fallible calls"
)]

use super::*;
use kraai_types::MessageContent;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Resolver(AtomicUsize);

#[async_trait::async_trait]
impl ImageResolver for Resolver {
    async fn resolve(&self, _image: &ImageAttachment) -> Result<Vec<u8>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![1, 2, 3])
    }
}

fn image() -> ImageAttachment {
    ImageAttachment {
        id: "a".repeat(64),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
        byte_length: 3,
    }
}

fn message(images: Vec<ImageAttachment>) -> ConversationItem {
    ConversationItem::User {
        content: MessageContent(
            images
                .into_iter()
                .map(|image| ContentPart::Image { image })
                .collect(),
        ),
    }
}

#[tokio::test]
async fn resolution_requires_resolver_and_preserves_deduplication() -> Result<()> {
    let messages = vec![message(vec![image(), image()])];
    assert!(
        ResolvedImages::resolve(&messages, &ProviderRequestContext::default())
            .await
            .is_err()
    );
    let resolver = Arc::new(Resolver(AtomicUsize::new(0)));
    let context = ProviderRequestContext::default().with_image_resolver(resolver.clone());
    let resolved = ResolvedImages::resolve(&messages, &context).await?;
    assert_eq!(resolved.data_url(&image())?, "data:image/png;base64,AQID");
    assert_eq!(resolver.0.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn conflicting_metadata_and_request_limits_fail() {
    let resolver = Arc::new(Resolver(AtomicUsize::new(0)));
    let context = ProviderRequestContext::default().with_image_resolver(resolver);
    let mut conflicting = image();
    conflicting.width = 2;
    for images in [vec![image(), conflicting], vec![image(); 33]] {
        assert!(
            ResolvedImages::resolve(&[message(images)], &context)
                .await
                .is_err()
        );
    }
    let mut large = image();
    large.byte_length = MAX_REQUEST_IMAGE_BYTES + 1;
    assert!(
        ResolvedImages::resolve(&[message(vec![large])], &context)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn aggregate_limits_reject_before_loading_any_image() {
    let resolver = Arc::new(Resolver(AtomicUsize::new(0)));
    let context = ProviderRequestContext::default().with_image_resolver(resolver.clone());
    let mut large = image();
    large.byte_length = kraai_types::image::MAX_IMAGE_BYTES as u64;
    assert!(
        ResolvedImages::resolve(&[message(vec![large; 4])], &context)
            .await
            .is_err()
    );
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
}

#[test]
fn unsupported_models_reject_images_but_accept_text() {
    let model = ModelId::new("text-only");
    assert!(validate_image_support(&[message(vec![image()])], &model, false).is_err());
    assert!(validate_image_support(&[message(vec![image()])], &model, true).is_ok());
    assert!(
        validate_image_support(
            &[ConversationItem::User {
                content: "hello".into()
            }],
            &model,
            false
        )
        .is_ok()
    );
}
