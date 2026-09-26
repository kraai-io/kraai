use super::*;
use kraai_types::{ContentPart, ImageAttachment, MessageContent};

fn send(harness: &mut TestHarness, text: &str) -> MessageContent {
    let message = MessageContent(vec![
        ContentPart::Text { text: text.into() },
        ContentPart::Image {
            image: ImageAttachment {
                id: "a".repeat(64),
                mime_type: "image/png".into(),
                width: 2,
                height: 3,
                byte_length: 100,
            },
        },
    ]);
    harness.app.dispatch_send_message(
        "old".into(),
        message.clone(),
        "model".into(),
        "provider".into(),
        crate::app::types::SubmissionSource::Pending,
    );
    message
}

#[test]
fn failed_send_is_recovered_only_in_its_session() {
    let mut harness = test_harness();
    harness.app.reset_chat_session(Some("old".into()), "old");
    let message = send(&mut harness, "old message");
    harness.app.start_new_chat();
    harness.app.set_input_text("new draft".into());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "old".into(),
            result: Err(kraai_runtime::RuntimeError::unavailable("failed")),
        });
    assert_eq!(harness.app.state.input, "new draft");
    assert_eq!(harness.app.state.draft_images.len(), 0);
    harness.app.reset_chat_session(Some("old".into()), "old");
    assert_eq!(
        harness.app.compose_message(&harness.app.state.input),
        message
    );
    assert!(harness.app.state.failed_messages.is_empty());
}

#[test]
fn background_failure_preserves_foreground_pending_send_and_stream() {
    let mut harness = test_harness();
    harness.app.reset_chat_session(Some("old".into()), "old");
    send(&mut harness, "old message");
    harness.app.reset_chat_session(Some("new".into()), "new");
    harness.app.dispatch_send_message(
        "new".into(),
        "new message".into(),
        "model".into(),
        "provider".into(),
        crate::app::types::SubmissionSource::Pending,
    );
    harness.app.set_input_text("next draft".into());
    let status = harness.app.state.status.clone();
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "old".into(),
            result: Err(kraai_runtime::RuntimeError::unavailable("failed")),
        });
    assert_eq!(harness.app.state.input, "next draft");
    assert_eq!(harness.app.state.status, status);
    assert!(harness.app.state.is_streaming);
    assert_eq!(harness.app.state.optimistic_messages.len(), 1);
    assert_eq!(harness.app.state.pending_messages.len(), 1);
    assert!(
        harness
            .app
            .state
            .pending_messages
            .front()
            .is_some_and(|pending| pending.session_id == "new")
    );
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "new".into(),
            result: Ok(kraai_runtime::SubmitMessageOutcome::Started {
                message_id: "new-message".into(),
            }),
        });
    assert!(harness.app.state.pending_messages.is_empty());
    assert_eq!(harness.app.state.input, "next draft");
}

#[test]
fn disconnected_background_submissions_recover_in_submission_order() {
    let mut harness = test_harness();
    harness.app.reset_chat_session(Some("old".into()), "old");
    let first = send(&mut harness, "first");
    let second = send(&mut harness, "second");
    harness.app.reset_chat_session(Some("new".into()), "new");
    harness.app.set_input_text("new draft".into());
    harness.app.handle_runtime_bridge_disconnect();
    assert_eq!(harness.app.state.input, "new draft");
    harness.app.reset_chat_session(Some("old".into()), "old");
    let mut expected = first.0;
    expected.push(ContentPart::Text {
        text: "\n\nsecond".into(),
    });
    expected.extend(
        second
            .0
            .into_iter()
            .filter(|part| matches!(part, ContentPart::Image { .. })),
    );
    assert_eq!(
        harness.app.compose_message(&harness.app.state.input),
        MessageContent(expected)
    );
}
