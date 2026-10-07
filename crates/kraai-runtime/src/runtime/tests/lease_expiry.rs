use std::time::Duration;

use color_eyre::eyre::Result;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn rejected_continuation_preserves_input_queued_before_a_foreign_claim() -> Result<()> {
    let observer = RuntimeTestHarness::new(Vec::new())
        .await
        .expect("observer fixture");
    let session = create_session_with_profile(&observer.handle, "test-profile").await?;
    observer
        .runtime
        .restore_queued_messages(
            &session,
            vec![super::super::core::QueuedMessage {
                message: "pending input".into(),
                model_id: kraai_types::ModelId::new("mock-model"),
                provider_id: kraai_types::ProviderId::new("mock"),
            }],
        )
        .await;
    let owner = kraai_persistence::Persistence::open(&observer.data_dir).await?;
    owner.sessions().claim_turn(&session).await?;
    assert!(
        observer
            .handle
            .continue_session(session.clone(), "mock-model".into(), "mock".into())
            .await
            .is_err()
    );
    let queued = observer.runtime.take_queued_messages(&session).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued.first().expect("queued input").message,
        "pending input".into()
    );
    assert!(owner.sessions().owns_turn(&session).await?);
    observer.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn observers_refresh_on_expiry_without_a_database_write_and_can_submit() -> Result<()> {
    let observer = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain("reply")]])
        .await
        .expect("observer fixture");
    let session = create_session_with_profile(&observer.handle, "test-profile").await?;
    let owner = kraai_persistence::Persistence::open(&observer.data_dir).await?;
    owner
        .sessions()
        .claim_turn_for(&session, Duration::from_millis(500))
        .await?;
    assert!(observer.handle.load_session(session.clone()).await?);
    let before = observer
        .handle
        .get_session_snapshot(session.clone())
        .await?;
    assert!(before.session.is_running);
    assert!(before.profiles.profile_locked);
    let revision = owner
        .sessions()
        .observe(&session)
        .await?
        .expect("lease")
        .revision;
    observer.events.wait_for("lease expiry update", |events| events.iter().any(|event| matches!(event, Event::HistoryUpdated { session_id } if session_id == &session))).await;
    let after = observer
        .handle
        .get_session_snapshot(session.clone())
        .await?;
    assert!(!after.session.is_running);
    assert!(!after.profiles.profile_locked);
    assert_eq!(
        owner
            .sessions()
            .observe(&session)
            .await?
            .expect("lease")
            .revision,
        revision
    );
    observer
        .handle
        .send_message(
            session.clone(),
            "new turn".into(),
            "mock-model".into(),
            "mock".into(),
        )
        .await?;
    observer.events.wait_for("turn completion", |events| events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id } if session_id == &session))).await;
    observer.shutdown().await;
    Ok(())
}
