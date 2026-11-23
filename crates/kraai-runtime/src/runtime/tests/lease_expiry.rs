use std::time::Duration;

use kraai_test_support::hang_guard;

use super::harness::FullHistory as _;
use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;
use kraai_persistence::SessionStore;

#[tokio::test]
async fn observers_refresh_on_expiry_without_a_revision_change_and_can_submit() {
    let observer = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain("reply")]]).await;
    let session = create_session_with_profile(&observer.handle, "test-profile").await;
    let owner = kraai_persistence::Persistence::open(&observer.data_dir)
        .await
        .unwrap();
    owner.sessions().claim_turn(&session).await.unwrap();
    assert!(observer.handle.load_session(session.clone()).await.unwrap());
    let before = observer
        .handle
        .get_session_snapshot(session.clone())
        .await
        .unwrap();
    assert!(before.session.is_running);
    assert!(before.profiles.profile_locked);
    let revision = owner
        .sessions()
        .observe(&session)
        .await
        .unwrap()
        .expect("lease")
        .revision;
    let connection = rusqlite::Connection::open(observer.data_dir.join("kraai.sqlite3")).unwrap();
    connection
        .busy_timeout(kraai_test_support::HANG_TIMEOUT)
        .unwrap();
    connection
        .execute(
            "UPDATE sessions SET lease_expires_at = 0 WHERE id = ?1",
            [session.as_str()],
        )
        .unwrap();
    let after = hang_guard(async {
        loop {
            let snapshot = observer
                .handle
                .get_session_snapshot(session.clone())
                .await?;
            if !snapshot.session.is_running {
                break Ok::<_, crate::RuntimeError>(snapshot);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert!(!after.session.is_running);
    assert!(!after.profiles.profile_locked);
    assert_eq!(
        owner
            .sessions()
            .observe(&session)
            .await
            .unwrap()
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
            Default::default(),
        )
        .await
        .unwrap();
    observer.events.wait_for("turn completion", |events| events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id } if session_id == &session))).await;
    observer.shutdown().await;
}

#[tokio::test]
async fn startup_recovers_sessions_whose_owner_stopped_mid_stream() {
    let directory = std::env::temp_dir().join(format!(
        "kraai-startup-sweep-{}-{}",
        std::process::id(),
        ulid::Ulid::generate()
    ));
    let session = kraai_types::SessionId::new("crashed");
    {
        let crashed = kraai_persistence::Persistence::open(&directory)
            .await
            .unwrap();
        crashed
            .sessions()
            .save(&kraai_persistence::SessionMeta {
                revision: 0,
                id: session.clone(),
                tip_id: None,
                workspace_dir: directory.join("workspace"),
                created_at: 1,
                updated_at: 1,
                title: None,
                selected_profile_id: Some("test-profile".into()),
                selected_model: None,
            })
            .await
            .unwrap();
        crashed.sessions().claim_turn(&session).await.unwrap();
        crashed
            .conversations()
            .append_messages(
                &session,
                None,
                vec![kraai_persistence::NewMessage {
                    id: kraai_types::MessageId::new("partial"),
                    content: kraai_types::ConversationItem::Assistant { items: Vec::new() },
                    status: kraai_types::MessageStatus::Streaming {
                        stream_id: kraai_types::StreamId::new("stream"),
                    },
                    agent_profile_id: None,
                    generation: None,
                }],
                None,
                None,
            )
            .await
            .unwrap();
    }
    let connection = rusqlite::Connection::open(directory.join("kraai.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE sessions SET lease_expires_at = 0 WHERE id = ?1",
            [session.as_str()],
        )
        .unwrap();

    let restarted = RuntimeTestHarness::new_shared(Vec::new(), directory.clone()).await;
    assert!(
        restarted
            .handle
            .get_chat_history(session.clone())
            .await
            .is_empty()
    );
    let lease_active: bool = connection
        .query_row(
            "SELECT lease_active FROM sessions WHERE id = ?1",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!lease_active);
    restarted.shutdown().await;
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

#[test]
fn a_timer_left_running_by_an_expired_lease_stops_at_the_expiry() {
    let now = std::time::Instant::now();
    let mut running = crate::TurnTimer::default();
    running.start(
        now.checked_sub(Duration::from_secs(60))
            .expect("clock too early"),
    );
    let expired_at = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(30))
        .expect("clock too early");
    let mut observation = kraai_persistence::SessionObservation {
        revision: 0,
        lease_active: true,
        lease_expires_at: i64::try_from(
            expired_at
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap(),
        turn_timer: Some(serde_json::to_string(&running).unwrap()),
    };
    let session = kraai_types::SessionId::new("session");
    let stored = crate::runtime::session_maintenance::stored_turn_timer(&session, &observation);
    assert!(stored.is_some_and(|timer| timer.elapsed(now).is_some()));

    observation.lease_active = false;
    let stopped = crate::runtime::session_maintenance::stored_turn_timer(&session, &observation)
        .expect("missing timer");
    assert_eq!(stopped.elapsed(std::time::Instant::now()), None);
    assert!(stopped.last_duration().is_some_and(|duration| {
        duration >= Duration::from_secs(29) && duration <= Duration::from_secs(31)
    }));
}
