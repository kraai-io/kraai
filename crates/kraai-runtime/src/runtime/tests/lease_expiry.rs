use std::time::Duration;

use color_eyre::eyre::Result;
use kraai_test_support::hang_guard;

use super::harness::FullHistory as _;
use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;
use kraai_persistence::SessionStore;

#[tokio::test]
async fn observers_refresh_on_expiry_without_a_revision_change_and_can_submit() -> Result<()> {
    let observer = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain("reply")]])
        .await
        .expect("observer fixture");
    let session = create_session_with_profile(&observer.handle, "test-profile").await?;
    let owner = kraai_persistence::Persistence::open(&observer.data_dir).await?;
    owner.sessions().claim_turn(&session).await?;
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
    let connection = rusqlite::Connection::open(observer.data_dir.join("kraai.sqlite3"))?;
    connection.busy_timeout(kraai_test_support::HANG_TIMEOUT)?;
    connection.execute(
        "UPDATE sessions SET lease_expires_at = 0 WHERE id = ?1",
        [session.as_str()],
    )?;
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
    .await??;
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
            Default::default(),
        )
        .await?;
    observer.events.wait_for("turn completion", |events| events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id } if session_id == &session))).await;
    observer.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn startup_recovers_sessions_whose_owner_stopped_mid_stream() -> Result<()> {
    let directory = std::env::temp_dir().join(format!(
        "kraai-startup-sweep-{}-{}",
        std::process::id(),
        ulid::Ulid::generate()
    ));
    let session = kraai_types::SessionId::new("crashed");
    {
        let crashed = kraai_persistence::Persistence::open(&directory).await?;
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
            .await?;
        crashed.sessions().claim_turn(&session).await?;
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
            .await?;
    }
    let connection = rusqlite::Connection::open(directory.join("kraai.sqlite3"))?;
    connection.execute(
        "UPDATE sessions SET lease_expires_at = 0 WHERE id = ?1",
        [session.as_str()],
    )?;

    let restarted = RuntimeTestHarness::new_shared(Vec::new(), directory.clone())
        .await
        .expect("restarted runtime");
    assert!(
        restarted
            .handle
            .get_chat_history(session.clone())
            .await?
            .is_empty()
    );
    let lease_active: bool = connection.query_row(
        "SELECT lease_active FROM sessions WHERE id = ?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    assert!(!lease_active);
    restarted.shutdown().await;
    tokio::fs::remove_dir_all(directory).await?;
    Ok(())
}
