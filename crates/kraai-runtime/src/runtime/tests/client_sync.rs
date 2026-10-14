use color_eyre::eyre::Result;
use kraai_types::{AssistantItem, ConversationItem, MessageId};

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::{Event, SessionActivity};

#[tokio::test]
async fn a_turn_reaches_clients_as_item_events_and_message_upserts() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::reasoning(serde_json::json!({"secret": "opaque"})),
        ScriptedChunk::plain("Looking"),
        ScriptedChunk::plain(" done."),
    ]])
    .await
    .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            "hello".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    let events = harness
        .events
        .wait_for("turn completion", |events| {
            events.iter().any(|event| {
                matches!(event, Event::TurnCompleted { session_id: id } if id == &session_id)
            })
        })
        .await;
    let session_events: Vec<_> = events
        .iter()
        .filter(|event| event.session_id() == Some(&session_id))
        .collect();

    let started: Vec<_> = session_events
        .iter()
        .filter_map(|event| match event {
            Event::ItemStarted {
                item_index, item, ..
            } => Some((*item_index, item.clone())),
            _ => None,
        })
        .collect();
    assert!(matches!(
        started.first(),
        Some((0, AssistantItem::Reasoning { payload, .. })) if payload.is_null()
    ));
    assert!(matches!(
        started.get(1),
        Some((1, AssistantItem::Text { text, .. })) if text == "Looking"
    ));
    assert_eq!(started.len(), 2);
    assert!(session_events.iter().any(|event| matches!(
        event,
        Event::ItemDelta { item_index: 1, delta, .. } if delta == " done."
    )));

    let upserted: Vec<_> = session_events
        .iter()
        .filter_map(|event| match event {
            Event::MessageUpserted { message, .. } => Some(message.as_ref().clone()),
            _ => None,
        })
        .collect();
    assert!(matches!(
        upserted.first().map(|message| &message.content),
        Some(ConversationItem::User { .. })
    ));
    let completed = upserted.last().expect("completed assistant message");
    assert_eq!(completed.status, kraai_types::MessageStatus::Complete);
    assert!(!serde_json::to_string(completed)?.contains("opaque"));
    assert!(
        !session_events
            .iter()
            .any(|event| matches!(event, Event::HistoryUpdated { .. }))
    );

    let states: Vec<_> = session_events
        .iter()
        .filter_map(|event| match event {
            Event::SessionStateChanged {
                activity,
                is_running,
                ..
            } => Some((*activity, *is_running)),
            _ => None,
        })
        .collect();
    assert!(states.contains(&(SessionActivity::Streaming, true)));
    assert_eq!(states.last(), Some(&(SessionActivity::Idle, false)));
    let idle = session_events.iter().position(|event| {
        matches!(
            event,
            Event::SessionStateChanged {
                is_running: false,
                ..
            }
        )
    });
    let turn_end = session_events
        .iter()
        .position(|event| matches!(event, Event::TurnCompleted { .. }));
    assert!(
        idle.is_some() && idle < turn_end,
        "clients must see the idle state before the turn's end"
    );

    let mut events = harness.runtime.event_tx.subscribe();
    harness
        .handle
        .undo_last_user_message(session_id.clone())
        .await?;
    let removed = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event.event {
            Event::MessagesRemoved { message_ids, .. } => Some(message_ids),
            _ => None,
        })
        .expect("removal event");
    assert_eq!(removed.len(), 2);
    assert!(removed.contains(&completed.id));
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn snapshots_carry_the_latest_history_and_pages_reach_the_first_message() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("runtime fixture");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let total = crate::runtime::session::SNAPSHOT_HISTORY_LIMIT + 5;
    let mut first = None;
    for index in 0..total {
        let message_id = MessageId::new(ulid::Ulid::generate());
        first.get_or_insert_with(|| message_id.clone());
        harness
            .runtime
            .agent_manager
            .add_script_result_to_history(
                &session_id,
                message_id,
                "test-profile".into(),
                kraai_types::ToolCallId::new(format!("call-{index}")),
                "output".into(),
                kraai_execution_types::ScriptExecutionOutcome {
                    status: kraai_execution_types::ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                },
            )
            .await?;
    }
    let snapshot = harness
        .handle
        .get_session_snapshot(session_id.clone())
        .await?;
    assert_eq!(
        snapshot.history.len(),
        crate::runtime::session::SNAPSHOT_HISTORY_LIMIT
    );
    let older = snapshot.older_history.expect("older history");
    let page = harness
        .handle
        .get_history_page(session_id.clone(), Some(older), 100)
        .await?;
    assert_eq!(page.messages.len(), 5);
    assert!(page.older_history.is_none());
    assert!(page.messages.contains_key(&first.expect("first message")));
    harness.shutdown().await;
    Ok(())
}
