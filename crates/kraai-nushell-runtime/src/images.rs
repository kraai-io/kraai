use std::future::Future;
use std::pin::Pin;

use kraai_types::ImageAttachment;

pub trait ImageAttachmentHandler: Send + Sync {
    fn attach(
        &self,
        sequence: u64,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, String>> + Send + '_>>;

    fn attach_existing(
        &self,
        sequence: u64,
        id: String,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, String>> + Send + '_>>;
}

#[derive(Debug, Default)]
pub struct RejectImageAttachments;

impl ImageAttachmentHandler for RejectImageAttachments {
    fn attach(
        &self,
        _sequence: u64,
        _bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, String>> + Send + '_>> {
        Box::pin(async {
            Err(String::from(
                "image attachments are not enabled for this execution",
            ))
        })
    }
    fn attach_existing(
        &self,
        _sequence: u64,
        _id: String,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, String>> + Send + '_>> {
        Box::pin(async {
            Err(String::from(
                "image attachments are not enabled for this execution",
            ))
        })
    }
}
