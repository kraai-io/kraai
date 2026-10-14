use std::collections::VecDeque;

use color_eyre::eyre::Result;
use kraai_provider_core::{ProviderManager, ProviderStreamEvent};
use kraai_types::{AssistantPhase, ProviderId};
use tokio::sync::{Mutex, mpsc};

use super::harness::{RuntimeTestHarness, create_session_with_profile};
use super::shared_sessions::ControlledProvider;
use crate::{Event, SubmitMessageOutcome};

#[tokio::test]
async fn takeover_discards_stale_streaming_state_before_releasing_the_old_lease() -> Result<()> {
    let (first_sender, first_receiver) = mpsc::unbounded_channel();
    let (next_sender, next_receiver) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(ControlledProvider {
            events: Mutex::new(VecDeque::from([first_receiver, next_receiver])),
        }),
    );
    let old_owner = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("old owner fixture");
    for task in &old_owner.maintenance_tasks {
        task.abort();
    }
    let (takeover_sender, takeover_receiver) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(ControlledProvider {
            events: Mutex::new(VecDeque::from([takeover_receiver])),
        }),
    );
    let new_owner =
        RuntimeTestHarness::new_in_directory(providers, None, Some(old_owner.data_dir.clone()))
            .await
            .expect("new owner fixture");
    let session = create_session_with_profile(&old_owner.handle, "test-profile").await?;
    old_owner
        .handle
        .send_message(
            session.clone(),
            "first request".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    first_sender.send(Ok(ProviderStreamEvent::TextDelta {
        item_id: "old-text".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "first partial response".into(),
    }))?;
    old_owner
        .events
        .wait_for("old stream started", |events| {
            events
                .iter()
                .any(|event| super::harness::streamed_text(event, &session).is_some())
        })
        .await;
    old_owner
        .runtime
        .agent_manager
        .publish_streaming_snapshots()
        .await?;
    let old_message = super::harness::started_stream(&old_owner.events.snapshot(), &session);
    let old_token = old_owner
        .runtime
        .session_store
        .owned_turns()
        .into_iter()
        .find(|(id, _)| id == &session)
        .expect("old lease")
        .1;
    let connection = rusqlite::Connection::open(old_owner.data_dir.join("kraai.sqlite3"))?;
    connection.busy_timeout(kraai_test_support::HANG_TIMEOUT)?;
    connection.execute(
        "UPDATE sessions SET lease_expires_at = 0 WHERE id = ?1",
        [session.as_str()],
    )?;
    new_owner
        .handle
        .send_message(
            session.clone(),
            "takeover request".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    assert!(
        new_owner
            .runtime
            .agent_manager
            .get_message(&old_message)
            .await?
            .is_none()
    );
    first_sender.send(Ok(ProviderStreamEvent::TextDelta {
        item_id: "old-text".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "stale response".into(),
    }))?;
    drop(first_sender);
    old_owner
        .events
        .wait_for("stale stream rejected", |events| {
            events.iter().any(|event| {
                matches!(event, Event::StreamError { session_id, .. } if session_id == &session)
            })
        })
        .await;
    old_owner.runtime.stream_tasks.wait_session(&session).await;
    super::harness::cancel_if_lease_matches(&old_owner.runtime, &session, old_token).await?;
    assert!(new_owner.runtime.session_store.owns_turn(&session).await?);
    takeover_sender.send(Ok(ProviderStreamEvent::TextDelta {
        item_id: "new-text".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "takeover response".into(),
    }))?;
    drop(takeover_sender);
    new_owner
        .events
        .wait_for("takeover completion", |events| {
            events.iter().any(|event| {
                matches!(event, Event::TurnCompleted { session_id } if session_id == &session)
            })
        })
        .await;
    old_owner
        .runtime
        .agent_manager
        .publish_streaming_snapshots()
        .await?;
    assert!(
        new_owner
            .runtime
            .agent_manager
            .get_message(&old_message)
            .await?
            .is_none(),
        "old client recreated its discarded streaming placeholder"
    );
    assert!(matches!(
        old_owner
            .handle
            .send_message(
                session.clone(),
                "next request".into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
            )
            .await?,
        SubmitMessageOutcome::Started { .. }
    ));
    next_sender.send(Ok(ProviderStreamEvent::TextDelta {
        item_id: "next-text".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "next response".into(),
    }))?;
    drop(next_sender);
    old_owner
        .events
        .wait_for("next completion", |events| {
            events.iter().any(|event| {
                matches!(event, Event::TurnCompleted { session_id } if session_id == &session)
            })
        })
        .await;
    new_owner.shutdown().await;
    old_owner.shutdown().await;
    Ok(())
}
