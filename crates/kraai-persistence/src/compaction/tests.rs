use super::*;
use std::sync::Arc;
use tokio::fs;

fn directory() -> PathBuf {
    std::env::temp_dir().join(format!("kraai-compaction-{}", ulid::Ulid::generate()))
}

fn checkpoint() -> CompactionCheckpoint {
    CompactionCheckpoint {
        covered_through: MessageId::new("boundary"),
        superseded_usage: vec![MessageId::new("latest")],
        previous_boundary: Some(MessageId::new("previous")),
        replacement: vec![ConversationItem::User {
            content: "User requested a parser; the parser is implemented.".into(),
        }],
        model_id: ModelId::new("model"),
        provider_id: ProviderId::new("provider"),
        prompt_version: 1,
        usage: Some(TokenUsage {
            input_tokens: 120,
            output_tokens: 15,
            ..TokenUsage::default()
        }),
    }
}

#[tokio::test]
async fn checkpoint_survives_restart_without_changing_history() -> Result<()> {
    let directory = directory();
    fs::create_dir_all(&directory).await?;
    let store = FileCompactionStore::new(&directory);
    let checkpoint = checkpoint();
    ensure!(store.get(&checkpoint.covered_through).await?.is_none());
    let history = directory.join("messages").join("boundary.json");
    atomic_replace_in_async(&directory, &history, b"original history")
        .await?
        .into_result()
        .map_err(color_eyre::Report::from)?;
    store.save(&checkpoint).await?;
    let reopened = FileCompactionStore::new(&directory);
    ensure!(reopened.get(&checkpoint.covered_through).await?.as_ref() == Some(&checkpoint));
    ensure!(fs::read(&history).await? == b"original history");
    fs::remove_dir_all(directory).await?;
    Ok(())
}

#[tokio::test]
async fn rejects_unsafe_ids_and_invalid_checkpoints_without_replacing_saved_state() -> Result<()> {
    let directory = directory();
    fs::create_dir_all(&directory).await?;
    let store = FileCompactionStore::new(&directory);
    let original = checkpoint();
    store.save(&original).await?;
    for raw in ["../escape", "/tmp/escape", r"..\escape", "C:escape", ""] {
        let id = MessageId(Arc::from(raw));
        ensure!(store.get(&id).await.is_err());
        let mut invalid = original.clone();
        invalid.covered_through = id.clone();
        ensure!(store.save(&invalid).await.is_err());
        invalid.covered_through = original.covered_through.clone();
        invalid.previous_boundary = Some(id);
        ensure!(store.save(&invalid).await.is_err());
    }
    let mut invalid = original.clone();
    invalid.replacement.clear();
    ensure!(store.save(&invalid).await.is_err());
    invalid = original.clone();
    invalid.prompt_version = 2;
    ensure!(store.save(&invalid).await.is_err());
    invalid = original.clone();
    invalid.previous_boundary = Some(invalid.covered_through.clone());
    ensure!(store.save(&invalid).await.is_err());
    ensure!(store.get(&original.covered_through).await?.as_ref() == Some(&original));
    fs::remove_dir_all(directory).await?;
    Ok(())
}

#[tokio::test]
async fn rejects_corrupt_or_mismatched_files() -> Result<()> {
    let directory = directory();
    fs::create_dir_all(&directory).await?;
    let store = FileCompactionStore::new(&directory);
    let checkpoint = checkpoint();
    store.save(&checkpoint).await?;
    let other = MessageId::new("other");
    atomic_replace_in_async(
        &directory,
        &store.path(&other)?,
        &serde_json::to_vec(&checkpoint)?,
    )
    .await?
    .into_result()
    .map_err(color_eyre::Report::from)?;
    ensure!(store.get(&other).await.is_err());
    for bytes in [b"{".as_slice(), b"null"] {
        fs::write(store.path(&other)?, bytes).await?;
        ensure!(store.get(&other).await.is_err());
    }
    let mut invalid = checkpoint.clone();
    invalid.replacement.clear();
    fs::write(
        store.path(&checkpoint.covered_through)?,
        serde_json::to_vec(&invalid)?,
    )
    .await?;
    ensure!(store.get(&checkpoint.covered_through).await.is_err());
    invalid = checkpoint.clone();
    invalid.prompt_version = 0;
    fs::write(
        store.path(&checkpoint.covered_through)?,
        serde_json::to_vec(&invalid)?,
    )
    .await?;
    ensure!(store.get(&checkpoint.covered_through).await.is_err());
    fs::remove_dir_all(directory).await?;
    Ok(())
}

#[tokio::test]
async fn failed_write_keeps_previous_checkpoint_readable() -> Result<()> {
    let directory = directory();
    fs::create_dir_all(&directory).await?;
    let store = FileCompactionStore::new(&directory);
    let original = checkpoint();
    store.save(&original).await?;
    let mut next = original.clone();
    next.covered_through = MessageId::new("next");
    next.previous_boundary = Some(original.covered_through.clone());
    fs::create_dir(store.path(&next.covered_through)?).await?;
    ensure!(store.save(&next).await.is_err());
    ensure!(store.get(&original.covered_through).await?.as_ref() == Some(&original));
    let mut entries = fs::read_dir(&store.root).await?;
    while let Some(entry) = entries.next_entry().await? {
        ensure!(
            entry
                .path()
                .extension()
                .is_none_or(|extension| extension != "tmp")
        );
    }
    fs::remove_dir_all(directory).await?;
    Ok(())
}

#[tokio::test]
async fn failed_checkpoint_deletion_preserves_source_message() -> Result<()> {
    use crate::{Persistence, SessionStore};
    use kraai_types::{ConversationItem, Message, MessageStatus};

    let directory = directory();
    fs::create_dir_all(&directory).await?;
    let persistence = Persistence::open(&directory).await?;
    let store = persistence.compactions;
    let messages = persistence.messages;
    let sessions = persistence.sessions;
    let checkpoint = checkpoint();
    let message = Message {
        id: checkpoint.covered_through.clone(),
        parent_id: None,
        content: ConversationItem::User {
            content: String::from("hello").into(),
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    };
    messages.save(&message).await?;
    let path = store.path(&message.id)?;
    fs::create_dir_all(&path).await?;
    ensure!(
        sessions
            .delete_message_if_unreferenced(&message.id, messages.clone())
            .await
            .is_err()
    );
    let retained = messages
        .get(&message.id)
        .await?
        .ok_or_else(|| eyre!("Source message was removed after checkpoint deletion failed"))?;
    ensure!(retained.id == message.id);
    ensure!(retained.content == message.content);
    ensure!(messages.exists(&message.id).await?);
    fs::remove_dir(&path).await?;
    store.save(&checkpoint).await?;
    sessions
        .delete_message_if_unreferenced(&message.id, messages.clone())
        .await?;
    ensure!(messages.get(&message.id).await?.is_none());
    ensure!(store.get(&message.id).await?.is_none());
    sessions
        .delete_message_if_unreferenced(&message.id, messages.clone())
        .await?;
    fs::remove_dir_all(directory).await?;
    Ok(())
}

#[tokio::test]
async fn session_deletion_waits_for_cancelled_compaction_commit_with_custom_messages() -> Result<()>
{
    use crate::{FileMessageStore, Persistence, SessionMeta, SessionStore};
    use kraai_types::{Message, MessageStatus};

    let directory = tempfile::tempdir()?;
    fs::create_dir_all(directory.path().join("custom-messages")).await?;
    let persistence = Persistence::open_with_messages(
        &directory.path().join("state"),
        Arc::new(FileMessageStore::new(
            &directory.path().join("custom-messages"),
        )),
    )
    .await?;
    let checkpoint = checkpoint();
    persistence
        .messages
        .save(&Message {
            id: checkpoint.covered_through.clone(),
            parent_id: None,
            content: ConversationItem::User {
                content: "work".into(),
            },
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        })
        .await?;
    persistence
        .sessions
        .save(&SessionMeta {
            id: "session".into(),
            tip_id: Some(checkpoint.covered_through.clone()),
            workspace_dir: directory.path().into(),
            created_at: 1,
            updated_at: 1,
            title: None,
            selected_profile_id: None,
            selected_model: None,
        })
        .await?;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let caller = tokio::spawn({
        let store = persistence.compactions.clone();
        let checkpoint = checkpoint.clone();
        async move {
            let path = store.path(&checkpoint.covered_through)?;
            let anchor = store.anchor.clone();
            let bytes = serde_json::to_vec(&checkpoint)?;
            store
                .commit(&checkpoint.covered_through, None, async move {
                    let _ = entered_tx.send(());
                    release_rx.await?;
                    Ok(atomic_replace_in_async(&anchor, &path, &bytes)
                        .await?
                        .into_result()?)
                })
                .await
        }
    });
    entered_rx.await?;
    caller.abort();
    let cancelled = caller.await.is_err_and(|error| error.is_cancelled());
    ensure!(cancelled);
    let deleting = persistence.delete_session("session");
    tokio::pin!(deleting);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let pending = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(deleting.as_mut().poll(cx).is_pending())
            })
            .await;
            ensure!(pending);
            if persistence.sessions.get("session").await?.is_none() {
                return Ok::<_, color_eyre::Report>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    ensure!(
        persistence
            .messages
            .exists(&checkpoint.covered_through)
            .await?
    );
    let waiting = tokio::time::timeout(std::time::Duration::from_millis(50), &mut deleting)
        .await
        .is_err();
    ensure!(waiting);
    release_tx
        .send(())
        .map_err(|()| eyre!("Compaction commit stopped before release"))?;
    tokio::time::timeout(std::time::Duration::from_secs(5), deleting).await??;
    drop(
        persistence
            .compactions
            .locks
            .lock(&checkpoint.covered_through)
            .await,
    );
    ensure!(
        !persistence
            .messages
            .exists(&checkpoint.covered_through)
            .await?
    );
    ensure!(
        persistence
            .compactions
            .get(&checkpoint.covered_through)
            .await?
            .is_none()
    );
    ensure!(
        !directory
            .path()
            .join("state/session-deletions/session.json")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_compaction_commit_holds_shutdown_barrier_until_durable() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = FileCompactionStore::new(directory.path());
    let checkpoint = checkpoint();
    let barrier = Arc::new(tokio::sync::RwLock::new(()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let caller = tokio::spawn({
        let store = store.clone();
        let checkpoint = checkpoint.clone();
        let barrier = barrier.clone();
        async move {
            let path = store.path(&checkpoint.covered_through)?;
            let anchor = store.anchor.clone();
            let bytes = serde_json::to_vec(&checkpoint)?;
            store
                .commit(&checkpoint.covered_through, Some(barrier), async move {
                    let _ = entered_tx.send(());
                    release_rx.await?;
                    Ok(atomic_replace_in_async(&anchor, &path, &bytes)
                        .await?
                        .into_result()?)
                })
                .await
        }
    });
    entered_rx.await?;
    caller.abort();
    let cancelled = caller.await.is_err_and(|error| error.is_cancelled());
    ensure!(cancelled);
    ensure!(barrier.try_write().is_err());
    release_tx
        .send(())
        .map_err(|()| eyre!("Compaction stopped before release"))?;
    let guard = tokio::time::timeout(std::time::Duration::from_secs(5), barrier.write()).await?;
    drop(guard);
    let reopened = FileCompactionStore::new(directory.path());
    ensure!(reopened.get(&checkpoint.covered_through).await?.as_ref() == Some(&checkpoint));
    Ok(())
}
