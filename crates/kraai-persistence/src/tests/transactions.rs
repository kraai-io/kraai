use super::*;

#[tokio::test]
async fn lease_and_usage_updates_do_not_invalidate_metadata_writes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("session")).await?;
    let root = persistence
        .conversations()
        .append_message(request("session", "root"))
        .await?;
    let mut metadata = persistence
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    persistence.sessions().claim_turn("session").await?;
    persistence
        .usage()
        .save(
            "session",
            &kraai_types::RequestUsage {
                message_id: root.message.id.clone(),
                provider_id: kraai_types::ProviderId::new("provider"),
                model_id: kraai_types::ModelId::new("model"),
                started_at: 1,
                subscription: false,
                unpriced_attempts: 0,
                usage: None,
            },
        )
        .await?;
    metadata.title = Some("changed-title".into());
    persistence.sessions().save(&metadata).await?;
    let mut metadata = persistence
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    persistence.messages().save(&root.message).await?;
    let mut message = root.message.clone();
    message.id = MessageId::new("new-message");
    message.parent_id = Some(root.message.id.clone());
    metadata.tip_id = Some(message.id.clone());
    let linked = persistence
        .messages()
        .save_linked(
            &message,
            &metadata,
            Some(&root.message.id),
            persistence.sessions().clone(),
        )
        .await?;
    ensure!(linked);
    let saved = persistence
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    ensure!(saved.title.as_deref() == Some("changed-title"));
    ensure!(saved.tip_id.as_ref() == Some(&message.id));
    ensure!(
        persistence
            .usage()
            .load("session")
            .await?
            .contains_key(&root.message.id)
    );
    Ok(())
}

#[tokio::test]
async fn stale_tip_conditional_save_preserves_concurrent_metadata_changes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    let b = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    let root = a
        .conversations()
        .append_message(request("session", "root"))
        .await?;
    let mut stale = a
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    let mut current = b
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    current.selected_profile_id = Some("changed-profile".into());
    current.workspace_dir = PathBuf::from("/changed-workspace");
    b.sessions().save(&current).await?;
    stale.tip_id = None;
    let result = a
        .sessions()
        .save_if_tip_matches(&stale, Some(&root.message.id))
        .await;
    ensure!(result.is_err());
    let saved = b
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    ensure!(saved.tip_id.as_ref() == Some(&root.message.id));
    ensure!(saved.selected_profile_id == current.selected_profile_id);
    ensure!(saved.workspace_dir == current.workspace_dir);
    Ok(())
}

#[tokio::test]
async fn stale_metadata_cannot_resurrect_a_deleted_session() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    let b = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    a.conversations()
        .append_message(request("session", "root"))
        .await?;
    let mut stale = a
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    b.sessions().delete("session").await?;
    stale.title = Some("late update".into());
    let result = a.sessions().save(&stale).await;
    ensure!(result.is_err());
    ensure!(b.sessions().get("session").await?.is_none());
    ensure!(b.messages().list_ids().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn stale_linked_message_does_not_overwrite_concurrent_metadata() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    let b = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    let root = a
        .conversations()
        .append_message(request("session", "root"))
        .await?;
    let mut stale = a
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    let mut current = b
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    current.selected_profile_id = Some("changed-profile".into());
    b.sessions().save(&current).await?;
    let mut message = root.message.clone();
    message.id = MessageId::new("new-message");
    message.parent_id = Some(root.message.id.clone());
    stale.tip_id = Some(message.id.clone());
    let result = a
        .messages()
        .save_linked(
            &message,
            &stale,
            Some(&root.message.id),
            a.sessions().clone(),
        )
        .await;
    ensure!(result.is_err());
    let saved = b
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    ensure!(saved.tip_id.as_ref() == Some(&root.message.id));
    ensure!(saved.selected_profile_id == current.selected_profile_id);
    ensure!(b.messages().get(&message.id).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn failed_release_stops_renewing_the_abandoned_claim() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("session")).await?;
    persistence
        .sessions()
        .claim_turn_for("session", Duration::from_millis(1))
        .await?;
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3"))?;
    connection.execute_batch("CREATE TRIGGER fail_release BEFORE UPDATE OF lease_active ON sessions WHEN NEW.lease_active = 0 BEGIN SELECT RAISE(FAIL, 'injected release failure'); END;")?;
    ensure!(
        persistence
            .sessions()
            .release_turn("session")
            .await
            .is_err()
    );
    ensure!(persistence.sessions().owned_sessions().await?.is_empty());
    ensure!(persistence.sessions().renew_turn("session").await.is_err());
    tokio::time::sleep(Duration::from_millis(5)).await;
    let observer = Persistence::open(directory.path()).await?;
    observer.sessions().claim_turn("session").await?;
    Ok(())
}

#[tokio::test]
async fn failed_tip_commit_rolls_back_the_message_and_title() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("session")).await?;
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3"))?;
    connection.execute_batch("CREATE TRIGGER fail_tip BEFORE UPDATE OF tip_id ON sessions BEGIN SELECT RAISE(FAIL, 'injected tip failure'); END;")?;
    ensure!(
        persistence
            .conversations()
            .append_message(request("session", "failed"))
            .await
            .is_err()
    );
    ensure!(persistence.messages().list_ids().await?.is_empty());
    let saved = persistence
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    ensure!(saved.tip_id.is_none() && saved.title.is_none());
    connection.execute_batch("DROP TRIGGER fail_tip")?;
    persistence
        .conversations()
        .append_message(request("session", "accepted"))
        .await?;
    Ok(())
}

#[tokio::test]
async fn failed_execution_finish_preserves_both_output_streams_and_running_phase() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("session")).await?;
    let record = persistence
        .executions()
        .create(execution("session"))
        .await?;
    persistence.executions().mark_running(&record.id).await?;
    for stream in [ScriptOutputStream::Stdout, ScriptOutputStream::Stderr] {
        persistence
            .executions()
            .append_output(&record.id, stream, b"prefix".to_vec())
            .await?;
    }
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3"))?;
    connection.execute_batch("CREATE TRIGGER fail_finish BEFORE INSERT ON records WHEN NEW.kind = 'execution' BEGIN SELECT RAISE(FAIL, 'injected execution failure'); END;")?;
    ensure!(
        persistence
            .executions()
            .finish(
                &record.id,
                ScriptExecutionCompletion {
                    status: ScriptExecutionStatus::Completed,
                    exit_code: Some(0),
                    sandbox_denied: false,
                    error: None,
                    stdout: b"replacement".to_vec(),
                    stderr: Vec::new(),
                }
            )
            .await
            .is_err()
    );
    ensure!(
        persistence
            .executions()
            .get(&record.id)
            .await?
            .is_some_and(|record| record.phase == ScriptExecutionPhase::Running)
    );
    let output = persistence.executions().read_output(&record.id).await?;
    ensure!(output.stdout == b"prefix" && output.stderr == b"prefix");
    Ok(())
}

#[tokio::test]
async fn retries_link_one_stable_message_and_reject_changed_content() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("session")).await?;
    let conversations = persistence.conversations();
    let id = MessageId::new("stable");
    let (first, second) = tokio::join!(
        conversations.append_message_idempotent(id.clone(), request("session", "result")),
        conversations.append_message_idempotent(id.clone(), request("session", "result")),
    );
    let first = first?;
    let second = second?;
    ensure!(first.linked_now != second.linked_now);
    ensure!(persistence.messages().list_ids().await?.len() == 1);
    conversations
        .append_message(request("session", "later"))
        .await?;
    ensure!(
        !conversations
            .append_message_idempotent(id.clone(), request("session", "result"))
            .await?
            .linked_now
    );
    ensure!(
        conversations
            .append_message_idempotent(id.clone(), request("session", "changed"))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn failed_session_deletion_restores_history_and_execution_data() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("session")).await?;
    let message = persistence
        .conversations()
        .append_message(request("session", "retained"))
        .await?;
    let execution = persistence
        .executions()
        .create(execution("session"))
        .await?;
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3"))?;
    connection.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON execution_sources BEGIN SELECT RAISE(FAIL, 'injected deletion failure'); END;")?;
    ensure!(persistence.sessions().delete("session").await.is_err());
    ensure!(persistence.sessions().get("session").await?.is_some());
    ensure!(
        persistence
            .messages()
            .get(&message.message.id)
            .await?
            .is_some()
    );
    ensure!(persistence.executions().get(&execution.id).await?.is_some());
    ensure!(persistence.executions().read_source(&execution.id).await? == b"print hello");
    connection.execute_batch("DROP TRIGGER fail_delete")?;
    persistence.sessions().delete("session").await?;
    ensure!(persistence.messages().list_ids().await?.is_empty());
    ensure!(persistence.executions().list_all().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn compaction_validation_preserves_the_saved_checkpoint() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("session")).await?;
    let message = persistence
        .conversations()
        .append_message(request("session", "original"))
        .await?;
    let checkpoint = CompactionCheckpoint {
        covered_through: message.message.id.clone(),
        superseded_usage: Vec::new(),
        previous_boundary: None,
        replacement: vec![ConversationItem::User {
            content: "summary".into(),
        }],
        model_id: kraai_types::ModelId::new("model"),
        provider_id: kraai_types::ProviderId::new("provider"),
        prompt_version: 1,
        usage: None,
    };
    persistence.compactions().save(&checkpoint).await?;
    for invalid in [
        CompactionCheckpoint {
            replacement: Vec::new(),
            ..checkpoint.clone()
        },
        CompactionCheckpoint {
            prompt_version: 0,
            ..checkpoint.clone()
        },
        CompactionCheckpoint {
            previous_boundary: Some(checkpoint.covered_through.clone()),
            ..checkpoint.clone()
        },
    ] {
        ensure!(persistence.compactions().save(&invalid).await.is_err());
    }
    let reopened = Persistence::open(directory.path()).await?;
    ensure!(
        reopened
            .compactions()
            .get(&checkpoint.covered_through)
            .await?
            .as_ref()
            == Some(&checkpoint)
    );
    ensure!(
        reopened
            .messages()
            .get(&message.message.id)
            .await?
            .is_some_and(|message| message.content.text() == Some("original"))
    );
    persistence.sessions().claim_turn("session").await?;
    let before = reopened
        .sessions()
        .observe("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    ensure!(
        reopened
            .compactions()
            .delete(&checkpoint.covered_through)
            .await
            .is_err()
    );
    persistence
        .compactions()
        .delete(&checkpoint.covered_through)
        .await?;
    let after = reopened
        .sessions()
        .observe("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    ensure!(after.revision == before.revision + 1);
    ensure!(
        reopened
            .compactions()
            .get(&checkpoint.covered_through)
            .await?
            .is_none()
    );
    Ok(())
}
