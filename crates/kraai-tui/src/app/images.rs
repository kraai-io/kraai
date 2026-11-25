use super::draft_images;
use super::types::RuntimeRequest;
use super::{App, RuntimeRequestDelivery};
use kraai_execution_types::ImageAttachment;
use kraai_execution_types::image::MAX_IMAGE_ATTACHMENTS;
use kraai_types::{MessageContent, SessionId};

impl App {
    pub(super) fn paste_image(&mut self) {
        if self.state.composer.images.pending() {
            return;
        }
        if self.state.composer.images.len() >= MAX_IMAGE_ATTACHMENTS {
            return;
        }
        let request_id = self.state.composer.next_image_request_id;
        self.state.composer.next_image_request_id += 1;
        if self.request(RuntimeRequest::PasteImage { request_id })
            == RuntimeRequestDelivery::Delivered
        {
            self.reset_input_history_navigation();
            self.state.composer.cursor = self.state.composer.images.insert(
                &mut self.state.composer.text,
                self.state.composer.cursor,
                request_id,
                None,
            );
        }
    }

    pub(super) fn compose_message(&self, text: &str) -> MessageContent {
        if self.state.composer.images.len() == 0 {
            text.into()
        } else {
            self.state
                .composer
                .images
                .content(&self.state.composer.text)
        }
    }

    pub(super) fn clear_message_draft(&mut self) {
        self.set_input_text(String::new());
    }

    pub(super) fn restore_message_draft(&mut self, message: MessageContent) {
        let (text, images) = draft_images::DraftImages::from_content(message);
        self.set_input_text(text);
        self.state.composer.images = images;
    }

    pub(super) fn recover_session_message(
        &mut self,
        session_id: Option<SessionId>,
        message: MessageContent,
    ) {
        self.recover_session_messages(session_id, vec![message]);
    }

    pub(super) fn recover_session_messages(
        &mut self,
        session_id: Option<SessionId>,
        messages: Vec<MessageContent>,
    ) {
        if self.state.current_session_id == session_id {
            for message in messages.into_iter().rev() {
                self.recover_message_draft(message);
            }
        } else {
            self.state
                .sessions
                .failed_messages
                .entry(session_id)
                .or_default()
                .extend(messages);
        }
    }

    pub(super) fn recover_message_draft(&mut self, message: MessageContent) {
        let (mut text, mut images) = draft_images::DraftImages::from_content(message);
        if !text.is_empty() && !self.state.composer.text.is_empty() {
            text.push_str("\n\n");
        }
        images.append_draft(
            &mut text,
            &self.state.composer.text,
            &self.state.composer.images,
        );
        self.set_input_text(text);
        self.state.composer.images = images;
    }

    pub(super) fn finish_image_import(
        &mut self,
        request_id: u64,
        result: kraai_runtime::RuntimeResult<Option<ImageAttachment>>,
    ) {
        let Some(range) = self.state.composer.images.pending_range(request_id) else {
            return;
        };
        match result {
            Ok(Some(image)) => {
                self.state.composer.images.finish(request_id, image);
            }
            Ok(None) | Err(_) => {
                let cursor = self.state.composer.cursor;
                let end = range.end;
                let start = range.start;
                self.state
                    .composer
                    .images
                    .replace(&mut self.state.composer.text, range, "");
                self.state.composer.cursor = if cursor >= end {
                    cursor - (end - start)
                } else {
                    cursor.min(start)
                };
                self.state.composer.cursor = self
                    .state
                    .composer
                    .images
                    .renumber(&mut self.state.composer.text, self.state.composer.cursor);
                if let Err(error) = result {
                    self.set_error(format!("Image paste failed: {error}"));
                }
            }
        }
    }

    pub(super) fn replace_input_range(&mut self, range: std::ops::Range<usize>, text: &str) {
        self.reset_input_history_navigation();
        self.state.composer.command_filter = None;
        self.state.composer.cursor =
            self.state
                .composer
                .images
                .replace(&mut self.state.composer.text, range, text);
        self.state.composer.cursor = self
            .state
            .composer
            .images
            .renumber(&mut self.state.composer.text, self.state.composer.cursor);
    }
}
