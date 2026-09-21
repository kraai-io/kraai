use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use kraai_types::{ConversationItem, MessageGeneration, ModelId, ProviderId, StreamId, TokenUsage};

use super::*;

#[derive(Clone, Default)]
struct RecordedOutput(Arc<Mutex<Vec<u8>>>);

impl Write for RecordedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn stream_event(sequence: u64, event: Event) -> RuntimeEventBridgeMessage {
    RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence, event })
}

fn snapshot_with_message(sequence: u64, complete: bool) -> SessionSnapshot {
    let mut snapshot = session_snapshot_at(sequence, None);
    let id = MessageId::new("answer");
    snapshot.session.tip_id = Some(id.to_string());
    snapshot.session.is_streaming = !complete;
    snapshot.session.is_running = !complete;
    snapshot.session.profile_locked = !complete;
    snapshot.activity = if complete {
        SessionActivity::Idle
    } else {
        SessionActivity::Streaming
    };
    snapshot.history.insert(
        id.clone(),
        Message {
            id,
            parent_id: None,
            content: ConversationItem::Assistant {
                items: vec![kraai_types::AssistantItem::Text {
                    phase: kraai_types::AssistantPhase::FinalAnswer,
                    text: String::from("answer text"),
                }],
            },
            status: if complete {
                MessageStatus::Complete
            } else {
                MessageStatus::Streaming {
                    stream_id: StreamId::new("stream"),
                }
            },
            agent_profile_id: None,
            generation: Some(MessageGeneration {
                provider_id: ProviderId::new("provider"),
                model_id: ModelId::new("model"),
                max_context: Some(4096),
                usage: complete.then_some(TokenUsage {
                    input_tokens: 20,
                    output_tokens: 3,
                    ..Default::default()
                }),
            }),
        },
    );
    snapshot
}

#[test]
fn snapshot_cannot_swallow_ci_stream_output() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.current_session_id = Some(String::from("session"));
    let output = RecordedOutput::default();
    harness.app.ci_output = Box::new(output.clone());
    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            1,
            Event::StreamStart {
                session_id: String::from("session"),
                message_id: String::from("answer"),
            },
        ));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(snapshot_with_message(3, false))),
        });

    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            2,
            Event::StreamChunk {
                session_id: String::from("session"),
                message_id: String::from("answer"),
                chunk: String::from("answer text"),
            },
        ));

    assert_eq!(
        *output.0.lock().unwrap_or_else(|error| error.into_inner()),
        b"answer text"
    );
}

#[test]
fn snapshot_cannot_swallow_ci_completion_and_usage() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.current_session_id = Some(String::from("session"));
    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            1,
            Event::StreamStart {
                session_id: String::from("session"),
                message_id: String::from("answer"),
            },
        ));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(snapshot_with_message(4, true))),
        });

    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            3,
            Event::StreamComplete {
                session_id: String::from("session"),
                message_id: String::from("answer"),
            },
        ));
    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            4,
            Event::HistoryUpdated {
                session_id: String::from("session"),
            },
        ));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(snapshot_with_message(4, true))),
        });

    assert!(harness.app.state.exit);
    assert_eq!(harness.app.evaluation_metrics()["turns"], 1);
    assert_eq!(
        harness.app.evaluation_metrics()["usage"]["input_tokens"],
        20
    );
}

#[test]
fn snapshot_cannot_swallow_a_ci_stream_failure() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.current_session_id = Some(String::from("session"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(session_snapshot_at(4, None))),
        });

    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            3,
            Event::StreamError {
                session_id: String::from("session"),
                message_id: String::from("answer"),
                error: String::from("provider disconnected"),
            },
        ));

    assert!(harness.app.state.exit);
    assert_eq!(
        harness.app.ci_error.as_deref(),
        Some("Stream error: provider disconnected")
    );
}

#[test]
fn ci_waits_for_the_entire_snapshot_event_backlog() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.current_session_id = Some(String::from("session"));
    let output = RecordedOutput::default();
    harness.app.ci_output = Box::new(output.clone());
    let mut snapshot = snapshot_with_message(8, true);
    let mut first_message = snapshot
        .history
        .get(&MessageId::new("answer"))
        .unwrap()
        .clone();
    first_message.id = MessageId::new("first");
    first_message.content = ConversationItem::Assistant {
        items: vec![kraai_types::AssistantItem::Text {
            phase: kraai_types::AssistantPhase::FinalAnswer,
            text: String::from("first"),
        }],
    };
    snapshot
        .history
        .insert(first_message.id.clone(), first_message);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(snapshot.clone())),
        });

    for (sequence, message_id, event_kind) in [
        (1, "first", 0),
        (2, "first", 1),
        (3, "first", 2),
        (4, "answer", 0),
        (5, "answer", 1),
        (6, "answer", 2),
    ] {
        let session_id = String::from("session");
        let message_id = String::from(message_id);
        let event = match event_kind {
            0 => Event::StreamStart {
                session_id,
                message_id,
            },
            1 => Event::StreamChunk {
                session_id,
                chunk: message_id.clone(),
                message_id,
            },
            _ => Event::StreamComplete {
                session_id,
                message_id,
            },
        };
        harness
            .app
            .handle_runtime_event_bridge_message(stream_event(sequence, event));
        if event_kind == 2 {
            harness
                .app
                .handle_runtime_response(RuntimeResponse::SessionSnapshot {
                    session_id: String::from("session"),
                    result: Box::new(Ok(snapshot.clone())),
                });
        }
        assert!(!harness.app.state.exit, "exited after event {sequence}");
        assert!(!harness.app.state.is_streaming);
        assert!(!harness.app.state.profile_locked);
    }

    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            7,
            Event::HistoryUpdated {
                session_id: String::from("session"),
            },
        ));
    assert!(!harness.app.state.exit);
    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            8,
            Event::ContextStateChanged {
                session_id: String::from("background"),
                notifications: Vec::new(),
            },
        ));

    assert!(harness.app.state.exit);
    assert_eq!(*output.0.lock().unwrap(), b"first\nanswer\n");
    assert_eq!(harness.app.evaluation_metrics()["turns"], 2);
    assert_eq!(
        harness.app.evaluation_metrics()["usage"]["input_tokens"],
        40
    );
}

#[test]
fn duplicate_covered_events_do_not_repeat_output_or_completion_requests() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.current_session_id = Some(String::from("session"));
    let output = RecordedOutput::default();
    harness.app.ci_output = Box::new(output.clone());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(snapshot_with_message(4, true))),
        });
    for _ in 0..2 {
        harness
            .app
            .handle_runtime_event_bridge_message(stream_event(
                2,
                Event::StreamChunk {
                    session_id: String::from("session"),
                    message_id: String::from("answer"),
                    chunk: String::from("answer text"),
                },
            ));
    }
    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            3,
            Event::StreamComplete {
                session_id: String::from("session"),
                message_id: String::from("answer"),
            },
        ));
    assert!(!harness.drain_requests().is_empty());
    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            3,
            Event::StreamComplete {
                session_id: String::from("session"),
                message_id: String::from("answer"),
            },
        ));
    assert!(harness.drain_requests().is_empty());
    assert_eq!(*output.0.lock().unwrap(), b"answer text\n");
    assert_eq!(harness.app.evaluation_metrics()["turns"], 1);
}

#[test]
fn covered_stream_lifecycle_preserves_newer_approval_state() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(String::from("session"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(session_snapshot_at(10, Some(pending_script("current"))))),
        });
    for (index, event) in [
        Event::StreamStart {
            session_id: String::from("session"),
            message_id: String::from("answer"),
        },
        Event::StreamComplete {
            session_id: String::from("session"),
            message_id: String::from("answer"),
        },
        Event::ScriptApprovalRequested {
            session_id: String::from("session"),
            script: pending_script("old"),
        },
    ]
    .into_iter()
    .enumerate()
    {
        harness
            .app
            .handle_runtime_event_bridge_message(stream_event(index as u64 + 1, event));
        assert_eq!(
            harness.app.state.script_phase,
            ScriptPhase::AwaitingApproval
        );
        assert_eq!(
            harness
                .app
                .state
                .pending_script
                .as_ref()
                .unwrap()
                .execution_id,
            "current"
        );
        assert!(!harness.app.state.is_streaming);
        assert!(harness.app.state.profile_locked);
    }
}

#[test]
fn snapshot_cannot_swallow_a_ci_approval_failure() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.current_session_id = Some(String::from("session"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: String::from("session"),
            result: Box::new(Ok(session_snapshot_at(4, Some(pending_script("current"))))),
        });
    harness
        .app
        .handle_runtime_event_bridge_message(stream_event(
            3,
            Event::ScriptApprovalRequested {
                session_id: String::from("session"),
                script: pending_script("current"),
            },
        ));
    assert!(harness.app.state.exit);
    assert_eq!(
        harness.app.ci_error.as_deref(),
        Some("CI mode cannot answer a script capability escalation prompt")
    );
}
