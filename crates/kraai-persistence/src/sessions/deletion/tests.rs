use super::*;
use crate::{AppendMessageRequest, Persistence};
use kraai_types::{
    ContextStateMutation, ConversationItem, MessageStatus, ModelId, PinnedFileScope, ProviderId,
    RequestUsage,
};

async fn seed(persistence: &Persistence, id: &str) -> Result<MessageId> {
    persistence
        .sessions
        .save(&SessionMeta {
            id: id.to_string(),
            tip_id: None,
            workspace_dir: PathBuf::from("/workspace"),
            created_at: 1,
            updated_at: 1,
            title: None,
            selected_profile_id: None,
            selected_model: None,
        })
        .await?;
    let appended = persistence
        .conversations()
        .append_message(AppendMessageRequest {
            session_id: id.to_string(),
            content: ConversationItem::User {
                content: "hello".into(),
            },
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
            title_if_first_message: None,
        })
        .await?;
    persistence
        .context
        .append_runtime(
            id,
            "test",
            vec![ContextStateMutation::PinFile {
                path: PathBuf::from("/workspace/source.rs"),
                scope: PinnedFileScope::Host,
            }],
        )
        .await?;
    persistence
        .usage
        .save(
            id,
            &RequestUsage {
                message_id: appended.message.id.clone(),
                provider_id: ProviderId::new("provider"),
                model_id: ModelId::new("model"),
                started_at: 1,
                subscription: false,
                unpriced_attempts: 0,
                usage: None,
            },
        )
        .await?;
    Ok(appended.message.id)
}

#[tokio::test]
async fn failed_cleanup_stays_hidden_and_resumes_after_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    let id = seed(&persistence, "deleted").await?;
    let deleted = persistence.sessions.get("deleted").await?.unwrap();
    persistence.usage.load("deleted").await?;
    let context_path = directory.path().join("context-state/deleted.json");
    fs::remove_file(&context_path).await?;
    fs::create_dir(&context_path).await?;
    assert!(persistence.delete_session("deleted").await.is_err());
    assert!(persistence.sessions.get("deleted").await?.is_none());
    assert!(!persistence.sessions.list_ids().await?.contains("deleted"));
    assert!(persistence.sessions.save(&deleted).await.is_err());
    assert!(
        !persistence
            .sessions
            .save_if_tip_matches(&deleted, Some(&id))
            .await?
    );
    assert!(!persistence.messages.exists(&id).await?);
    assert!(
        directory
            .path()
            .join("session-deletions/deleted.json")
            .exists()
    );
    assert!(Persistence::open(directory.path()).await.is_err());
    fs::remove_dir(&context_path).await?;
    drop(persistence);
    let reopened = Persistence::open(directory.path()).await?;
    assert!(reopened.sessions.list().await?.is_empty());
    assert!(reopened.context.list("deleted").await?.is_empty());
    assert!(reopened.usage.load("deleted").await?.is_empty());
    assert!(
        !directory
            .path()
            .join("session-deletions/deleted.json")
            .exists()
    );
    reopened.delete_session("deleted").await?;
    Ok(())
}

#[tokio::test]
async fn recovery_at_each_commit_boundary_preserves_shared_ancestry() -> Result<()> {
    for metadata_removed in [false, true] {
        let directory = tempfile::tempdir()?;
        let persistence = Persistence::open(directory.path()).await?;
        let shared = seed(&persistence, "kept").await?;
        let mut deleted = persistence.sessions.get("kept").await?.unwrap();
        deleted.id = "deleted".into();
        persistence.sessions.save(&deleted).await?;
        let unique = persistence
            .conversations()
            .append_message(AppendMessageRequest {
                session_id: "deleted".into(),
                content: ConversationItem::User {
                    content: "unique".into(),
                },
                status: MessageStatus::Complete,
                agent_profile_id: None,
                generation: None,
                title_if_first_message: None,
            })
            .await?
            .message
            .id;
        let marker = directory.path().join("session-deletions/deleted.json");
        atomic_replace_in_async(
            directory.path(),
            &marker,
            &serde_json::to_vec(&SessionDeletion {
                session_id: "deleted".into(),
                messages: HashSet::from([shared.clone(), unique.clone()]),
            })?,
        )
        .await?
        .into_result()?;
        if metadata_removed {
            let mut remaining = persistence.sessions.state.sessions.read().await.clone();
            remaining.remove("deleted");
            SessionState::persist_sessions(&remaining, &directory.path().join("sessions.json"))
                .await?
                .into_result()?;
            persistence.messages.delete(&unique).await?;
        }
        drop(persistence);
        let reopened = Persistence::open(directory.path()).await?;
        assert!(reopened.sessions.get("deleted").await?.is_none());
        assert!(reopened.sessions.get("kept").await?.is_some());
        assert!(reopened.messages.exists(&shared).await?);
        assert!(!reopened.messages.exists(&unique).await?);
        assert!(!marker.exists());
    }
    Ok(())
}

#[tokio::test]
async fn marker_failure_keeps_session_visible_and_retry_clears_usage_cache() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    let message = seed(&persistence, "session").await?;
    assert_eq!(persistence.usage.load("session").await?.len(), 1);
    let markers = directory.path().join("session-deletions");
    fs::write(&markers, b"blocked").await?;
    assert!(persistence.delete_session("session").await.is_err());
    assert!(persistence.sessions.get("session").await?.is_some());
    assert!(persistence.messages.exists(&message).await?);
    fs::remove_file(&markers).await?;
    persistence.delete_session("session").await?;
    assert!(persistence.usage.load("session").await?.is_empty());
    assert!(persistence.context.list("session").await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn cancelled_deletion_finishes_all_records_before_releasing_ownership() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    let id = seed(&persistence, "session").await?;
    persistence.usage.load("session").await?;
    let cached = persistence.sessions.state.sessions.read().await;
    let caller = tokio::spawn({
        let persistence = persistence.clone();
        async move { persistence.delete_session("session").await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let sessions: HashMap<String, SessionMeta> =
                serde_json::from_slice(&fs::read(directory.path().join("sessions.json")).await?)?;
            if !sessions.contains_key("session") {
                return Ok::<_, color_eyre::Report>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(persistence.sessions.write_guard.try_lock().is_err());
    drop(cached);
    let guard = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        persistence.sessions.write_guard.lock(),
    )
    .await?;
    assert!(!persistence.messages.exists(&id).await?);
    assert!(persistence.context.list("session").await?.is_empty());
    assert!(persistence.usage.load("session").await?.is_empty());
    assert!(
        !directory
            .path()
            .join("session-deletions/session.json")
            .exists()
    );
    drop(guard);
    Ok(())
}

#[tokio::test]
async fn deletion_retains_execution_archives_and_shared_image_content() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    seed(&persistence, "session").await?;
    let archive = directory.path().join("executions/archive");
    let image = directory.path().join("images/shared-image");
    fs::create_dir_all(&archive).await?;
    fs::create_dir_all(image.parent().unwrap()).await?;
    fs::write(archive.join("source.nu"), b"echo retained").await?;
    fs::write(&image, b"shared content").await?;
    persistence.delete_session("session").await?;
    drop(persistence);
    Persistence::open(directory.path()).await?;
    assert_eq!(fs::read(archive.join("source.nu")).await?, b"echo retained");
    assert_eq!(fs::read(&image).await?, b"shared content");
    Ok(())
}
