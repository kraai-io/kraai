use kraai_types::{
    ConversationItem, MessageGeneration, ModelId, ProviderId, SessionId, StreamId, TokenUsage,
};

use super::*;

fn stream_event(sequence: u64, event: Event) -> RuntimeEventBridgeMessage {
    RuntimeEventBridgeMessage::Event(RuntimeEvent { sequence, event })
}

fn text_started_at(message_id: &str, index: usize, text: &str) -> Event {
    Event::ItemStarted {
        session_id: SessionId::new("session"),
        message_id: message_id.into(),
        item_index: index,
        item: kraai_types::AssistantItem::Text {
            phase: kraai_types::AssistantPhase::FinalAnswer,
            text: text.into(),
        },
    }
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
                options: Default::default(),
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
fn covered_stream_lifecycle_preserves_newer_approval_state() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: SessionId::new("session"),
            result: Box::new(Ok(session_snapshot_at(10, Some(pending_script("current"))))),
        });
    for (index, event) in [
        Event::StreamStart {
            session_id: SessionId::new("session"),
            message_id: String::from("answer"),
        },
        Event::StreamComplete {
            session_id: SessionId::new("session"),
            message_id: String::from("answer"),
        },
        Event::ScriptApprovalRequested {
            session_id: SessionId::new("session"),
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
fn item_events_build_the_streaming_message_and_gaps_request_a_snapshot() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness.app.state.chat_history = snapshot_with_message(1, false).history;
    harness.app.handle_runtime_event(Event::ItemDelta {
        session_id: SessionId::new("session"),
        message_id: "answer".into(),
        item_index: 0,
        delta: " continued".into(),
    });
    let call = Event::ItemStarted {
        session_id: SessionId::new("session"),
        message_id: "answer".into(),
        item_index: 1,
        item: kraai_types::AssistantItem::ScriptCall {
            call_id: kraai_types::ToolCallId::new("call"),
            name: "kraai_nushell".into(),
            input: "# timeout=1sec
print '</tool_call>'"
                .into(),
        },
    };
    harness.app.handle_runtime_event(call.clone());
    harness.app.handle_runtime_event(call);
    assert!(matches!(
        &harness.app.state.chat_history.get(&MessageId::new("answer")).unwrap().content,
        ConversationItem::Assistant { items }
            if matches!(items.as_slice(),
                [kraai_types::AssistantItem::Text { text, .. }, kraai_types::AssistantItem::ScriptCall { input, .. }]
                if text == "answer text continued" && input == "# timeout=1sec\nprint '</tool_call>'")
    ));
    harness.drain_requests();
    harness
        .app
        .handle_runtime_event(text_started_at("answer", 5, "lost"));
    assert!(harness.drain_requests().iter().any(|request| matches!(
        request,
        RuntimeRequest::GetSessionSnapshot { session_id } if session_id.as_str() == "session"
    )));
}

#[test]
fn history_events_upsert_and_remove_messages() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    let message = snapshot_with_message(1, true)
        .history
        .remove(&MessageId::new("answer"))
        .unwrap();
    harness.app.handle_runtime_event(Event::MessageUpserted {
        session_id: SessionId::new("session"),
        message: Box::new(message),
    });
    assert!(
        harness
            .app
            .state
            .chat_history
            .contains_key(&MessageId::new("answer"))
    );
    harness.app.handle_runtime_event(Event::MessagesRemoved {
        session_id: SessionId::new("session"),
        message_ids: vec![MessageId::new("answer")],
    });
    assert!(harness.app.state.chat_history.is_empty());
}

#[test]
fn loading_session_restores_model_without_overwriting_later_selection() {
    let mut harness = test_harness();
    harness
        .app
        .reset_chat_session(Some(SessionId::new("session")), "Session loaded");
    let mut snapshot = session_snapshot_at(1, None);
    snapshot.session.selected_model = Some(kraai_types::ModelSelection {
        provider_id: ProviderId::new("saved-provider"),
        model_id: ModelId::new("saved-model"),
        options: kraai_types::ModelOptionValues::from([(
            "effort".into(),
            kraai_types::ModelOptionValue::Choice("high".into()),
        )]),
    });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: SessionId::new("session"),
            result: Box::new(Ok(snapshot.clone())),
        });
    assert_eq!(
        harness.app.state.selected_provider_id.as_deref(),
        Some("saved-provider")
    );
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("saved-model")
    );
    harness.app.state.selected_model_id = Some("new-model".into());
    let draft_options = kraai_types::ModelOptionValues::from([(
        "effort".into(),
        kraai_types::ModelOptionValue::Choice("low".into()),
    )]);
    harness.app.state.selected_model_options = draft_options.clone();
    snapshot.event_sequence += 1;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: SessionId::new("session"),
            result: Box::new(Ok(snapshot)),
        });
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("new-model")
    );
    assert_eq!(harness.app.state.selected_model_options, draft_options);
}

#[test]
fn observer_refreshes_model_when_saved_selection_changes() {
    let mut harness = test_harness();
    harness
        .app
        .reset_chat_session(Some(SessionId::new("session")), "Session loaded");
    for (sequence, selection) in [
        None,
        Some(("first-provider", "first-model", "low")),
        Some(("second-provider", "second-model", "low")),
        Some(("second-provider", "second-model", "high")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut snapshot = session_snapshot_at(sequence as u64, None);
        snapshot.session.selected_model =
            selection.map(|(provider, model, effort)| kraai_types::ModelSelection {
                provider_id: ProviderId::new(provider),
                model_id: ModelId::new(model),
                options: kraai_types::ModelOptionValues::from([(
                    "effort".into(),
                    kraai_types::ModelOptionValue::Choice(effort.into()),
                )]),
            });
        harness
            .app
            .handle_runtime_response(RuntimeResponse::SessionSnapshot {
                model_save_id: 0,
                session_id: SessionId::new("session"),
                result: Box::new(Ok(snapshot)),
            });
        if let Some((provider, model, effort)) = selection {
            assert_eq!(
                harness.app.state.selected_provider_id.as_deref(),
                Some(provider)
            );
            assert_eq!(harness.app.state.selected_model_id.as_deref(), Some(model));
            assert_eq!(
                harness.app.state.selected_model_options.get("effort"),
                Some(&kraai_types::ModelOptionValue::Choice(effort.into()))
            );
        }
    }
}
