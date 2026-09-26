use super::*;
use kraai_types::image::{MAX_IMAGE_ATTACHMENTS, MAX_IMAGE_BYTES};
use tokio::io::AsyncReadExt;

pub(super) async fn read_image_file(
    path: &std::path::Path,
) -> kraai_runtime::RuntimeResult<Vec<u8>> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path).await.map_err(|error| {
        kraai_runtime::RuntimeError::unavailable(format!("Cannot open image: {error}"))
    })?;
    let metadata = file.metadata().await.map_err(|error| {
        kraai_runtime::RuntimeError::unavailable(format!("Cannot inspect image: {error}"))
    })?;
    if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err(kraai_runtime::RuntimeError::unavailable(format!(
            "Image must be a regular file of at most {MAX_IMAGE_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            kraai_runtime::RuntimeError::unavailable(format!("Cannot read image: {error}"))
        })?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(kraai_runtime::RuntimeError::unavailable(
            "Image exceeds size limit",
        ));
    }
    Ok(bytes)
}

fn content_text(content: &MessageContent) -> String {
    content.text_only().into_owned()
}

impl App {
    pub(super) fn attach_image(&mut self, path: &str) {
        if self.state.image_import_pending {
            self.state.status = String::from("An image is still loading");
            return;
        }
        if path == "clear" {
            let text = if self.state.input.trim() == "/image clear" {
                content_text(&self.state.draft_content)
            } else {
                self.state.input.clone()
            };
            self.restore_message_draft(text.into());
            self.state.status = String::from("Image attachments cleared");
            return;
        }
        if path.is_empty() {
            self.state.status = String::from("Usage: /image <path>, or /image clear");
            return;
        }
        if self.state.draft_content.images().count() >= MAX_IMAGE_ATTACHMENTS {
            self.state.status = format!("At most {MAX_IMAGE_ATTACHMENTS} images can be attached");
            return;
        }
        if self.request(RuntimeRequest::ImportImage {
            path: path.into(),
            session_id: self.state.current_session_id.clone(),
        }) == RuntimeRequestDelivery::Delivered
        {
            self.state.image_import_pending = true;
            self.set_input_text(String::new());
            self.state.status = String::from("Loading image attachment");
        }
    }

    pub(super) fn compose_message(&self, text: &str) -> MessageContent {
        if !self.state.draft_content.parts().is_empty()
            && content_text(&self.state.draft_content) == text
        {
            return self.state.draft_content.clone();
        }
        let mut parts = Vec::new();
        if !text.is_empty() {
            parts.push(ContentPart::Text {
                text: text.to_string(),
            });
        }
        parts.extend(
            self.state
                .draft_content
                .images()
                .cloned()
                .map(|image| ContentPart::Image { image }),
        );
        MessageContent(parts)
    }

    pub(super) fn clear_message_draft(&mut self) {
        self.state.draft_content = MessageContent::default();
        self.set_input_text(String::new());
    }

    pub(super) fn restore_message_draft(&mut self, message: MessageContent) {
        let text = content_text(&message);
        self.state.draft_content = message;
        self.set_input_text(text);
    }

    pub(super) fn recover_message_draft(&mut self, mut message: MessageContent) {
        let draft = self.compose_message(&self.state.input);
        if draft == message {
            return;
        }
        message.0.extend(draft.0);
        self.restore_message_draft(message);
    }

    pub(super) fn finish_image_import(
        &mut self,
        result: kraai_runtime::RuntimeResult<ImageAttachment>,
    ) {
        self.state.image_import_pending = false;
        match result {
            Ok(image) => {
                self.state.status = format!("Attached {}×{} image", image.width, image.height);
                self.state
                    .draft_content
                    .0
                    .push(ContentPart::Image { image });
            }
            Err(error) => self.set_error(format!("Image attachment failed: {error}")),
        }
    }
}
