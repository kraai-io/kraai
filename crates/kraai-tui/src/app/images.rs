use super::*;
use kraai_types::image::MAX_IMAGE_ATTACHMENTS;

impl App {
    pub(super) fn paste_image(&mut self) {
        if self.state.draft_images.pending() {
            self.state.status = String::from("An image is still loading");
            return;
        }
        if self.state.draft_images.len() >= MAX_IMAGE_ATTACHMENTS {
            self.state.status = format!("At most {MAX_IMAGE_ATTACHMENTS} images can be attached");
            return;
        }
        let request_id = self.state.next_image_request_id;
        self.state.next_image_request_id += 1;
        if self.request(RuntimeRequest::PasteImage { request_id })
            == RuntimeRequestDelivery::Delivered
        {
            self.reset_input_history_navigation();
            self.state.input_cursor = self.state.draft_images.insert(
                &mut self.state.input,
                self.state.input_cursor,
                request_id,
                None,
            );
            self.state.status = String::from("Loading clipboard image");
        }
    }

    pub(super) fn compose_message(&self, text: &str) -> MessageContent {
        if self.state.draft_images.len() == 0 {
            text.into()
        } else {
            self.state.draft_images.content(&self.state.input)
        }
    }

    pub(super) fn clear_message_draft(&mut self) {
        self.set_input_text(String::new());
    }

    pub(super) fn restore_message_draft(&mut self, message: MessageContent) {
        let (text, images) = draft_images::DraftImages::from_content(message);
        self.set_input_text(text);
        self.state.draft_images = images;
    }

    pub(super) fn recover_session_message(
        &mut self,
        session_id: Option<String>,
        message: MessageContent,
    ) {
        self.recover_session_messages(session_id, vec![message]);
    }

    pub(super) fn recover_session_messages(
        &mut self,
        session_id: Option<String>,
        messages: Vec<MessageContent>,
    ) {
        if self.state.current_session_id == session_id {
            for message in messages.into_iter().rev() {
                self.recover_message_draft(message);
            }
        } else {
            self.state
                .failed_messages
                .entry(session_id)
                .or_default()
                .extend(messages);
        }
    }

    pub(super) fn recover_message_draft(&mut self, message: MessageContent) {
        let (mut text, mut images) = draft_images::DraftImages::from_content(message);
        if !text.is_empty() && !self.state.input.is_empty() {
            text.push_str("\n\n");
        }
        images.append_draft(&mut text, &self.state.input, &self.state.draft_images);
        self.set_input_text(text);
        self.state.draft_images = images;
    }

    pub(super) fn finish_image_import(
        &mut self,
        request_id: u64,
        result: kraai_runtime::RuntimeResult<ImageAttachment>,
    ) {
        let Some(range) = self.state.draft_images.pending_range(request_id) else {
            return;
        };
        match result {
            Ok(image) => {
                self.state.status = format!("Attached {}×{} image", image.width, image.height);
                self.state.draft_images.finish(request_id, image);
            }
            Err(error) => {
                let cursor = self.state.input_cursor;
                let end = range.end;
                let start = range.start;
                self.state
                    .draft_images
                    .replace(&mut self.state.input, range, "");
                self.state.input_cursor = if cursor >= end {
                    cursor - (end - start)
                } else {
                    cursor.min(start)
                };
                self.state.input_cursor = self
                    .state
                    .draft_images
                    .renumber(&mut self.state.input, self.state.input_cursor);
                self.set_error(format!("Image paste failed: {error}"));
            }
        }
    }

    pub(super) fn replace_input_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        self.reset_input_history_navigation();
        self.state.input_cursor =
            self.state
                .draft_images
                .replace(&mut self.state.input, range, text);
        self.state.input_cursor = self
            .state
            .draft_images
            .renumber(&mut self.state.input, self.state.input_cursor);
    }
}
