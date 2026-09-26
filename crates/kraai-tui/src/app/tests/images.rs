use super::*;
use kraai_types::{ContentPart, ImageAttachment, MessageContent};

fn image() -> ImageAttachment {
    ImageAttachment {
        id: "a".repeat(64),
        mime_type: "image/png".to_string(),
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

#[test]
fn image_command_loads_paths_with_spaces_and_waits_before_sending() {
    let mut harness = test_harness();
    ready(&mut harness);
    harness
        .app
        .set_input_text("/image screenshot with spaces.png".into());
    harness.app.handle_submit();
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::ImportImage { path, session_id: None }] if path == std::path::Path::new("screenshot with spaces.png"))
    );
    harness.app.set_input_text("describe this".into());
    harness.app.handle_submit();
    assert!(harness.drain_requests().is_empty());
    assert_eq!(harness.app.state.input, "describe this");
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ImportImage {
            session_id: None,
            result: Ok(image()),
        });
    assert_eq!(harness.app.state.draft_content.images().count(), 1);
    harness.app.handle_submit();
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::CreateSession { .. }]
    ));
    let pending = harness.app.state.pending_submit.as_ref();
    assert_eq!(
        pending.map(|pending| pending.message.images().count()),
        Some(1)
    );
    assert!(!harness.app.state.draft_content.has_images());
}

#[test]
fn image_only_messages_restore_after_send_failure() {
    let mut harness = test_harness();
    ready(&mut harness);
    harness.app.state.current_session_id = Some("session".into());
    harness.app.finish_image_import(Ok(image()));
    harness.app.handle_submit();
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::SendMessage { message, .. }] if matches!(message.parts(), [ContentPart::Image { .. }]))
    );
    assert!(!harness.app.state.draft_content.has_images());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage(Err(
            kraai_runtime::RuntimeError::unavailable("fixture failure"),
        )));
    assert_eq!(harness.app.state.draft_content.images().count(), 1);
    assert!(harness.app.state.input.is_empty());
}

#[test]
fn image_import_captures_the_active_session_workspace() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some("other-workspace-session".into());
    harness.app.attach_image("screenshot.png");
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::ImportImage { session_id: Some(session_id), .. }] if session_id == "other-workspace-session")
    );
}

#[test]
fn image_import_is_discarded_after_switching_sessions() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some("first".into());
    harness.app.attach_image("screenshot.png");
    harness.app.state.current_session_id = Some("second".into());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ImportImage {
            session_id: Some("first".into()),
            result: Ok(image()),
        });
    assert!(!harness.app.state.image_import_pending);
    assert!(!harness.app.state.draft_content.has_images());
}

#[test]
fn clearing_images_preserves_restored_and_new_draft_text() {
    for command_input in [false, true] {
        let mut harness = test_harness();
        harness.app.restore_message_draft(MessageContent(vec![
            ContentPart::Text {
                text: "keep this question".into(),
            },
            ContentPart::Image { image: image() },
        ]));
        if command_input {
            harness.app.set_input_text("/image clear".into());
        }
        harness.app.attach_image("clear");
        assert_eq!(harness.app.state.input, "keep this question");
        assert!(!harness.app.state.draft_content.has_images());
    }
}

#[test]
fn disconnect_and_creation_failure_preserve_attachments() {
    for disconnect in [false, true] {
        let mut harness = test_harness();
        ready(&mut harness);
        harness.app.finish_image_import(Ok(image()));
        harness.app.set_input_text("question".into());
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
        assert_eq!(harness.app.state.draft_content.images().count(), 1);
        assert_eq!(harness.app.state.input, "question");
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
    harness.app.handle_submit();
    assert!(
        matches!(harness.drain_requests().as_slice(), [RuntimeRequest::SendMessage { message, .. }] if message == &original)
    );
}

#[test]
fn attachment_limit_and_config_errors_keep_the_draft() {
    let mut harness = test_harness();
    for _ in 0..kraai_types::image::MAX_IMAGE_ATTACHMENTS {
        harness.app.finish_image_import(Ok(image()));
    }
    harness.app.attach_image("one-too-many.png");
    assert!(harness.drain_requests().is_empty());
    harness.app.set_input_text("question".into());
    harness.app.handle_submit();
    assert_eq!(harness.app.state.input, "question");
    assert_eq!(
        harness.app.state.draft_content.images().count(),
        kraai_types::image::MAX_IMAGE_ATTACHMENTS
    );
}

#[tokio::test]
async fn file_import_rejects_oversized_files_and_directories() -> color_eyre::Result<()> {
    let directory = tempfile::tempdir()?;
    color_eyre::eyre::ensure!(
        crate::app::images::read_image_file(directory.path())
            .await
            .is_err()
    );
    let path = directory.path().join("large.png");
    let file = std::fs::File::create(&path)?;
    file.set_len(kraai_types::image::MAX_IMAGE_BYTES as u64 + 1)?;
    color_eyre::eyre::ensure!(crate::app::images::read_image_file(&path).await.is_err());
    Ok(())
}
