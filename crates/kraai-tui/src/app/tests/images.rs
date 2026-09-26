#![expect(
    clippy::panic_in_result_fn,
    reason = "image composer tests assert after fallible editor operations"
)]

use super::*;
use crate::app::{KeyCode, KeyEvent, KeyModifiers, is_known_slash_command};
use kraai_types::{ContentPart, ImageAttachment, MessageContent};

fn image() -> ImageAttachment {
    ImageAttachment {
        id: "a".repeat(64),
        mime_type: "image/png".into(),
        width: 2,
        height: 3,
        byte_length: 100,
    }
}

fn ready(harness: &mut TestHarness) {
    harness.app.state.config_loaded = true;
    harness.app.state.selected_provider_id = Some("provider".into());
    harness.app.state.selected_model_id = Some("model".into());
    harness.app.state.selected_profile_id = Some("agent".into());
}

fn begin_paste(harness: &mut TestHarness) -> u64 {
    let id = harness.app.state.next_image_request_id;
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::PasteImage { request_id }] if *request_id == id)
    );
    id
}

fn paste(harness: &mut TestHarness) {
    let request_id = begin_paste(harness);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::PasteImage {
            request_id,
            result: Ok(image()),
        });
}

#[test]
fn ctrl_v_inserts_at_cursor_without_losing_typing_during_import() {
    let mut harness = test_harness();
    ready(&mut harness);
    harness.app.set_input_text("before after".into());
    harness.app.state.input_cursor = 7;
    let id = begin_paste(&mut harness);
    assert_eq!(harness.app.state.input, "before [Image #1]after");
    harness.app.insert_input_text("during ");
    harness.app.handle_submit();
    assert!(harness.drain_requests().is_empty());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::PasteImage {
            request_id: id,
            result: Ok(image()),
        });
    let content = harness.app.compose_message(&harness.app.state.input);
    assert!(
        matches!(content.parts(), [ContentPart::Text { text: before }, ContentPart::Image { .. }, ContentPart::Text { text: after }] if before == "before " && after == "during after")
    );
    harness.app.handle_submit();
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::CreateSession { .. }]
    ));
    assert_eq!(
        harness
            .app
            .state
            .pending_submit
            .as_ref()
            .map(|pending| &pending.message),
        Some(&content)
    );
}

#[test]
fn ordinary_pasted_paths_and_chip_labels_stay_text_and_image_command_is_removed() {
    let mut harness = test_harness();
    let text = "/tmp/screenshot.png [Image #1]";
    harness.app.handle_paste(text.into());
    assert_eq!(
        harness.app.compose_message(text),
        MessageContent::from(text)
    );
    assert_eq!(harness.app.state.draft_images.len(), 0);
    assert!(harness.drain_requests().is_empty());
    assert!(!is_known_slash_command("image"));
}

#[test]
fn cursor_and_deletion_treat_chips_as_units() {
    for (key, cursor, expected) in [(KeyCode::Backspace, 11, "ab"), (KeyCode::Delete, 1, "ab")] {
        let mut harness = test_harness();
        harness.app.set_input_text("ab".into());
        harness.app.state.input_cursor = 1;
        paste(&mut harness);
        assert_eq!(harness.app.state.input, "a[Image #1]b");
        harness.app.move_input_cursor_left();
        assert_eq!(harness.app.state.input_cursor, 1);
        harness.app.move_input_cursor_right();
        assert_eq!(harness.app.state.input_cursor, 11);
        harness.app.state.input_cursor = cursor;
        harness
            .app
            .handle_key_event(KeyEvent::new(key, KeyModifiers::NONE));
        assert_eq!(harness.app.state.input, expected);
        assert_eq!(harness.app.state.draft_images.len(), 0);
    }
}

#[test]
fn word_deletion_and_unicode_edits_keep_remaining_image_order() {
    let mut harness = test_harness();
    harness.app.insert_input_text("é ");
    paste(&mut harness);
    harness.app.insert_input_text(" ");
    paste(&mut harness);
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(harness.app.state.input, "é [Image #1] ");
    assert_eq!(harness.app.state.draft_images.len(), 1);
    harness.app.state.input_cursor = 0;
    harness.app.insert_input_text("🦀");
    let content = harness.app.compose_message(&harness.app.state.input);
    assert!(
        matches!(content.parts(), [ContentPart::Text { text }, ContentPart::Image { .. }, ContentPart::Text { .. }] if text == "🦀é ")
    );
}

#[test]
fn late_import_cannot_restore_deleted_cleared_or_switched_chips() {
    for action in 0..3 {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some("first".into());
        let old = begin_paste(&mut harness);
        match action {
            0 => harness.app.backspace_input_char(),
            1 => harness.app.handle_ctrl_c(),
            _ => {
                harness.app.reset_chat_session(Some("second".into()), "");
                harness.app.reset_chat_session(Some("first".into()), "");
            }
        }
        let new = begin_paste(&mut harness);
        harness
            .app
            .handle_runtime_response(RuntimeResponse::PasteImage {
                request_id: old,
                result: Ok(image()),
            });
        assert!(harness.app.state.draft_images.pending());
        harness
            .app
            .handle_runtime_response(RuntimeResponse::PasteImage {
                request_id: new,
                result: Ok(image()),
            });
        assert!(!harness.app.state.draft_images.pending());
        assert_eq!(harness.app.state.draft_images.len(), 1);
    }
}

#[test]
fn failed_clipboard_read_removes_only_its_chip() {
    let mut harness = test_harness();
    harness.app.set_input_text("before after".into());
    harness.app.state.input_cursor = 7;
    let id = begin_paste(&mut harness);
    harness.app.insert_input_text("new ");
    harness.app.finish_image_import(
        id,
        Err(kraai_runtime::RuntimeError::unavailable("no image")),
    );
    assert_eq!(harness.app.state.input, "before new after");
    assert_eq!(harness.app.state.input_cursor, 11);
    assert!(!harness.app.state.draft_images.pending());
}

#[test]
fn history_recovers_draft_attachments_and_literal_labels_remain_text() {
    let mut harness = test_harness();
    harness.app.state.input_history = vec!["old [Image #1]".into()];
    harness.app.insert_input_text("question ");
    paste(&mut harness);
    let content = harness.app.compose_message(&harness.app.state.input);
    harness.app.handle_input_up();
    assert_eq!(harness.app.state.input, "old [Image #1]");
    assert_eq!(harness.app.state.draft_images.len(), 0);
    harness.app.handle_input_down();
    assert_eq!(
        harness.app.compose_message(&harness.app.state.input),
        content
    );
}

#[test]
fn image_only_messages_restore_after_send_failure() {
    let mut harness = test_harness();
    ready(&mut harness);
    harness.app.state.current_session_id = Some("session".into());
    paste(&mut harness);
    harness.app.handle_submit();
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::SendMessage { message, .. }] if matches!(message.parts(), [ContentPart::Image { .. }]))
    );
    assert_eq!(harness.app.state.draft_images.len(), 0);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "session".into(),
            result: Err(kraai_runtime::RuntimeError::unavailable("fixture failure")),
        });
    assert_eq!(harness.app.state.input, "[Image #1]");
    assert_eq!(harness.app.state.draft_images.len(), 1);
}

#[test]
fn disconnect_and_creation_failure_preserve_attachments() {
    for disconnect in [false, true] {
        let mut harness = test_harness();
        ready(&mut harness);
        harness.app.insert_input_text("question ");
        paste(&mut harness);
        harness.app.handle_submit();
        if disconnect {
            harness.app.handle_runtime_bridge_disconnect();
        } else {
            harness
                .app
                .handle_runtime_response(RuntimeResponse::CreateSession {
                    creation_id: 0,
                    result: Err(kraai_runtime::RuntimeError::unavailable("fixture failure")),
                });
        }
        assert_eq!(harness.app.state.draft_images.len(), 1);
        assert_eq!(harness.app.state.input, "question [Image #1]");
    }
}

#[test]
fn undo_preserves_ordered_content_when_resubmitted() {
    let mut harness = test_harness();
    ready(&mut harness);
    harness.app.state.current_session_id = Some("session".into());
    let original = MessageContent(vec![
        ContentPart::Text {
            text: "first".into(),
        },
        ContentPart::Image { image: image() },
        ContentPart::Text {
            text: "last".into(),
        },
    ]);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::UndoLastUserMessage {
            session_id: "session".into(),
            result: Ok(Some(original.clone())),
        });
    harness.drain_requests();
    assert_eq!(harness.app.state.input, "first[Image #1]last");
    harness.app.handle_submit();
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::SendMessage { message, .. }] if message == &original)
    );
}

#[test]
fn attachment_limit_keeps_the_draft() {
    let mut harness = test_harness();
    for _ in 0..kraai_types::image::MAX_IMAGE_ATTACHMENTS {
        paste(&mut harness);
    }
    harness.app.paste_image();
    assert!(harness.drain_requests().is_empty());
    harness.app.insert_input_text("question");
    harness.app.handle_submit();
    assert!(harness.app.state.input.ends_with("question"));
    assert_eq!(
        harness.app.state.draft_images.len(),
        kraai_types::image::MAX_IMAGE_ATTACHMENTS
    );
}

#[test]
fn editor_preserves_reorders_and_removes_existing_chips() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    paste(&mut harness);
    harness.app.insert_input_text(" between ");
    paste(&mut harness);
    let original = harness.app.state.input.clone();
    let (_, labels) = harness.app.state.draft_images.editor(&original);
    let reordered = labels
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join(" new ");
    harness
        .app
        .apply_edited_prompt(reordered, None, &original, &labels)?;
    assert_eq!(harness.app.state.draft_images.len(), 2);
    let original = harness.app.state.input.clone();
    let (_, labels) = harness.app.state.draft_images.editor(&original);
    let duplicate = labels.iter().take(1).cloned().collect::<String>().repeat(2);
    assert!(
        harness
            .app
            .apply_edited_prompt(duplicate, None, &original, &labels)
            .is_err()
    );
    harness
        .app
        .apply_edited_prompt("only text".into(), None, &original, &labels)?;
    assert_eq!(harness.app.state.draft_images.len(), 0);
    Ok(())
}

#[test]
fn automatic_session_creation_preserves_next_draft_and_pending_paste() {
    let mut harness = test_harness();
    ready(&mut harness);
    harness.app.set_input_text("first message".into());
    harness.app.handle_submit();
    harness.drain_requests();
    harness.app.insert_input_text("next draft ");
    let request_id = begin_paste(&mut harness);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::CreateSession {
            creation_id: 0,
            result: Ok("created".into()),
        });
    assert_eq!(harness.app.state.input, "next draft [Image #1]");
    harness.app.finish_image_import(request_id, Ok(image()));
    assert_eq!(
        harness
            .app
            .compose_message(&harness.app.state.input)
            .images()
            .count(),
        1
    );
}

#[test]
fn recovery_preserves_pending_chip_and_completion_identity() {
    let mut harness = test_harness();
    let request_id = begin_paste(&mut harness);
    harness
        .app
        .recover_message_draft(MessageContent(vec![ContentPart::Image { image: image() }]));
    assert_eq!(harness.app.state.input, "[Image #1]\n\n[Image #2]");
    assert!(harness.app.state.draft_images.pending());
    harness.app.finish_image_import(request_id, Ok(image()));
    assert_eq!(
        harness
            .app
            .compose_message(&harness.app.state.input)
            .images()
            .count(),
        2
    );
}

#[test]
fn editor_cannot_rebind_image_to_a_literal_label() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    paste(&mut harness);
    harness.app.insert_input_text(" literal [Image #1]");
    let original = harness.app.state.input.clone();
    let (editor_text, labels) = harness.app.state.draft_images.editor(&original);
    harness
        .app
        .apply_edited_prompt(editor_text, None, &original, &labels)?;
    assert_eq!(harness.app.state.input, original);
    assert_eq!(harness.app.state.draft_images.len(), 1);
    harness
        .app
        .apply_edited_prompt(" literal [Image #1]".into(), None, &original, &labels)?;
    assert_eq!(harness.app.state.input, " literal [Image #1]");
    assert_eq!(harness.app.state.draft_images.len(), 0);
    Ok(())
}

#[test]
fn image_numbers_follow_submitted_order_after_insertion_deletion_and_editor_reordering()
-> color_eyre::Result<()> {
    let mut harness = test_harness();
    let mut second = image();
    second.id = "b".repeat(64);
    harness
        .app
        .restore_message_draft(MessageContent(vec![ContentPart::Image {
            image: second.clone(),
        }]));
    harness.app.state.input_cursor = 0;
    paste(&mut harness);
    assert_eq!(harness.app.state.input, "[Image #1][Image #2]");
    assert_eq!(harness.app.state.input_cursor, 10);
    let original = harness.app.state.input.clone();
    let (_, labels) = harness.app.state.draft_images.editor(&original);
    let reordered = labels.iter().rev().cloned().collect::<String>();
    harness
        .app
        .apply_edited_prompt(reordered, None, &original, &labels)?;
    assert_eq!(harness.app.state.input, "[Image #1][Image #2]");
    assert_eq!(
        harness
            .app
            .compose_message(&harness.app.state.input)
            .images()
            .next(),
        Some(&second)
    );
    harness.app.state.input_cursor = 0;
    harness.app.delete_input_char();
    assert_eq!(harness.app.state.input, "[Image #1]");
    assert_eq!(
        harness
            .app
            .compose_message(&harness.app.state.input)
            .images()
            .next(),
        Some(&image())
    );
    Ok(())
}

#[test]
fn new_chat_clears_a_sessionless_draft_and_ignores_its_pending_image() {
    let mut harness = test_harness();
    let id = begin_paste(&mut harness);
    harness.app.start_new_chat();
    harness.app.finish_image_import(id, Ok(image()));
    assert!(harness.app.state.input.is_empty());
    assert_eq!(harness.app.state.draft_images.len(), 0);
}

#[test]
fn automatic_submission_cannot_clear_an_identical_new_draft() {
    let mut harness = test_harness();
    ready(&mut harness);
    harness.app.set_input_text("same message".into());
    harness.app.handle_submit();
    harness.drain_requests();
    harness.app.set_input_text("same message".into());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::CreateSession {
            creation_id: 0,
            result: Ok("created".into()),
        });
    assert_eq!(harness.app.state.input, "same message");
}

#[test]
fn failed_submission_does_not_merge_an_identical_independent_draft() {
    let mut harness = test_harness();
    ready(&mut harness);
    harness.app.state.current_session_id = Some("session".into());
    harness.app.set_input_text("same message".into());
    harness.app.handle_submit();
    harness.app.set_input_text("same message".into());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "session".into(),
            result: Err(kraai_runtime::RuntimeError::unavailable("failed")),
        });
    assert_eq!(harness.app.state.input, "same message\n\nsame message");
}

#[test]
fn editor_distinguishes_literal_labels_and_marker_collisions() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    harness
        .app
        .insert_input_text("literal [Image #1] [Image #1; kraai:0] before ");
    paste(&mut harness);
    harness.app.insert_input_text(" after [Image #1]");
    let original = harness.app.state.input.clone();
    let content = harness.app.compose_message(&original);
    let (editor_text, labels) = harness.app.state.draft_images.editor(&original);
    harness.app.apply_edited_prompt(
        editor_text.replace("before", "edited before"),
        None,
        &original,
        &labels,
    )?;
    let edited = harness.app.compose_message(&harness.app.state.input);
    assert_eq!(
        edited.images().collect::<Vec<_>>(),
        content.images().collect::<Vec<_>>()
    );
    assert!(
        edited
            .text_only()
            .contains("literal [Image #1] [Image #1; kraai:0] edited before")
    );
    assert!(edited.text_only().ends_with(" after [Image #1]"));
    Ok(())
}

#[test]
fn editor_keeps_an_image_that_finishes_loading_while_open() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    let id = begin_paste(&mut harness);
    let original = harness.app.state.input.clone();
    let (editor_text, labels) = harness.app.state.draft_images.editor(&original);
    harness.app.finish_image_import(id, Ok(image()));
    harness
        .app
        .apply_edited_prompt(editor_text, None, &original, &labels)?;
    assert_eq!(
        harness
            .app
            .compose_message(&harness.app.state.input)
            .images()
            .next(),
        Some(&image())
    );
    Ok(())
}
