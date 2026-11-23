use super::*;

#[tokio::test]
async fn a_pooled_read_sees_one_commit_across_its_queries() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    let path = directory.path().join("kraai.sqlite3");
    let (before, after) = persistence
        .sessions()
        .sqlite_database()
        .read(move |connection, _| {
            let revision = |connection: &rusqlite::Connection| {
                connection.query_row(
                    "SELECT revision FROM sessions WHERE id = 'session'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
            };
            let before = revision(connection)?;
            rusqlite::Connection::open(&path)?.execute(
                "UPDATE sessions SET revision = revision + 1 WHERE id = 'session'",
                [],
            )?;
            Ok((before, revision(connection)?))
        })
        .await
        .unwrap();
    assert!(
        before == after,
        "A read observed a commit made between its queries"
    );
}

#[tokio::test]
async fn active_lease_rejection_does_not_require_the_database_write_lock() {
    let directory = tempfile::tempdir().unwrap();
    let owner = open_persistence(directory.path()).await.unwrap();
    owner.sessions().save(&session("session")).await.unwrap();
    owner
        .sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    let observer = open_persistence(directory.path()).await.unwrap();
    observer
        .sessions()
        .sqlite_database()
        .run(|connection, _| {
            connection.busy_timeout(Duration::ZERO)?;
            Ok(())
        })
        .await
        .unwrap();
    let mut connection =
        rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let error = observer
        .sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .expect_err("An active foreign lease must reject the claim");
    assert!(
        error
            .downcast_ref::<kraai_execution_types::DomainError>()
            .is_some_and(|error| error.kind() == kraai_execution_types::DomainErrorKind::Conflict),
        "Expected an ownership conflict while the writer is held, got {error}"
    );
    transaction.rollback().unwrap();
    assert!(
        owner
            .sessions()
            .owns_turn(&SessionId::new("session"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn lease_and_usage_updates_do_not_invalidate_metadata_writes() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    let root = persistence
        .conversations()
        .append_message(request("session", "root"))
        .await
        .unwrap();
    let mut metadata = persistence
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    persistence
        .sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    persistence
        .usage()
        .save(
            &SessionId::new("session"),
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
        .await
        .unwrap();
    metadata.title = Some("changed-title".into());
    persistence.sessions().save(&metadata).await.unwrap();
    let mut metadata = persistence
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    persistence.messages().save(&root.message).await.unwrap();
    let mut message = root.message.clone();
    message.id = MessageId::new("new-message");
    message.parent_id = Some(root.message.id.clone());
    metadata.tip_id = Some(message.id.clone());
    let linked = persistence
        .messages()
        .save_linked(LinkRequest {
            messages: std::slice::from_ref(&message),
            session: &metadata,
            expected_tip: Some(&root.message.id),
            consumed_queue_through: None,
        })
        .await
        .unwrap();
    assert!(linked == Some(true));
    let saved = persistence
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    assert!(saved.title.as_deref() == Some("changed-title"));
    assert!(saved.tip_id.as_ref() == Some(&message.id));
    assert!(
        persistence
            .usage()
            .load(&SessionId::new("session"))
            .await
            .unwrap()
            .contains_key(&root.message.id)
    );
}

#[tokio::test]
async fn stale_tip_conditional_save_preserves_concurrent_metadata_changes() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    let root = a
        .conversations()
        .append_message(request("session", "root"))
        .await
        .unwrap();
    let mut stale = a
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    let mut current = b
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    current.selected_profile_id = Some("changed-profile".into());
    current.workspace_dir = PathBuf::from("/changed-workspace");
    b.sessions().save(&current).await.unwrap();
    stale.tip_id = None;
    let result = a
        .sessions()
        .save_if_tip_matches(&stale, Some(&root.message.id))
        .await;
    assert!(result.is_err());
    let saved = b
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    assert!(saved.tip_id.as_ref() == Some(&root.message.id));
    assert!(saved.selected_profile_id == current.selected_profile_id);
    assert!(saved.workspace_dir == current.workspace_dir);
}

#[tokio::test]
async fn stale_metadata_cannot_resurrect_a_deleted_session() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    a.conversations()
        .append_message(request("session", "root"))
        .await
        .unwrap();
    let mut stale = a
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    b.sessions()
        .delete(&SessionId::new("session"))
        .await
        .unwrap();
    stale.title = Some("late update".into());
    let result = a.sessions().save(&stale).await;
    assert!(result.is_err());
    assert!(
        b.sessions()
            .get(&SessionId::new("session"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(b.messages().list_ids().await.unwrap().is_empty());
}

#[tokio::test]
async fn stale_linked_message_does_not_overwrite_concurrent_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    let root = a
        .conversations()
        .append_message(request("session", "root"))
        .await
        .unwrap();
    let mut stale = a
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    let mut current = b
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    current.selected_profile_id = Some("changed-profile".into());
    b.sessions().save(&current).await.unwrap();
    let mut message = root.message.clone();
    message.id = MessageId::new("new-message");
    message.parent_id = Some(root.message.id.clone());
    stale.tip_id = Some(message.id.clone());
    let result = a
        .messages()
        .save_linked(LinkRequest {
            messages: std::slice::from_ref(&message),
            session: &stale,
            expected_tip: Some(&root.message.id),
            consumed_queue_through: None,
        })
        .await;
    assert!(result.is_err());
    let saved = b
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    assert!(saved.tip_id.as_ref() == Some(&root.message.id));
    assert!(saved.selected_profile_id == current.selected_profile_id);
    assert!(b.messages().get(&message.id).await.unwrap().is_none());
}

#[tokio::test]
async fn failed_release_stops_renewing_the_abandoned_claim() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    persistence
        .sessions()
        .claim_turn_for(&SessionId::new("session"), Duration::from_millis(1))
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_release BEFORE UPDATE OF lease_active ON sessions WHEN NEW.lease_active = 0 BEGIN SELECT RAISE(FAIL, 'injected release failure'); END;").unwrap();
    assert!(
        persistence
            .sessions()
            .release_turn(&SessionId::new("session"))
            .await
            .is_err()
    );
    assert!(persistence.sessions().owned_sessions().is_empty());
    assert!(
        persistence
            .sessions()
            .renew_turn(&SessionId::new("session"))
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    let observer = open_persistence(directory.path()).await.unwrap();
    observer
        .sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_tip_commit_rolls_back_the_message_and_title() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_tip BEFORE UPDATE OF tip_id ON sessions BEGIN SELECT RAISE(FAIL, 'injected tip failure'); END;").unwrap();
    assert!(
        persistence
            .conversations()
            .append_message(request("session", "failed"))
            .await
            .is_err()
    );
    assert!(persistence.messages().list_ids().await.unwrap().is_empty());
    let saved = persistence
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    assert!(saved.tip_id.is_none() && saved.title.is_none());
    connection.execute_batch("DROP TRIGGER fail_tip").unwrap();
    persistence
        .conversations()
        .append_message(request("session", "accepted"))
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_execution_finish_preserves_both_output_streams_and_running_phase() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    let record = persistence
        .executions()
        .create(execution("session"))
        .await
        .unwrap();
    persistence
        .executions()
        .mark_running(&record.id)
        .await
        .unwrap();
    for stream in [ScriptOutputStream::Stdout, ScriptOutputStream::Stderr] {
        persistence
            .executions()
            .append_output(&record.id, vec![(stream, b"prefix".to_vec())])
            .await
            .unwrap();
    }
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_finish BEFORE INSERT ON records WHEN NEW.kind = 'execution' BEGIN SELECT RAISE(FAIL, 'injected execution failure'); END;").unwrap();
    assert!(
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
    assert!(
        persistence
            .executions()
            .get(&record.id)
            .await
            .unwrap()
            .is_some_and(|record| record.phase == ScriptExecutionPhase::Running)
    );
    let output = persistence
        .executions()
        .read_output(&record.id)
        .await
        .unwrap();
    assert!(output.stdout == b"prefix" && output.stderr == b"prefix");
}

#[tokio::test]
async fn retries_link_one_stable_message_and_reject_changed_content() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    let conversations = persistence.conversations();
    let id = MessageId::new("stable");
    let (first, second) = tokio::join!(
        conversations.append_message_idempotent(id.clone(), request("session", "result")),
        conversations.append_message_idempotent(id.clone(), request("session", "result")),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert!(first.linked_now != second.linked_now);
    assert!(persistence.messages().list_ids().await.unwrap().len() == 1);
    conversations
        .append_message(request("session", "later"))
        .await
        .unwrap();
    assert!(
        !conversations
            .append_message_idempotent(id.clone(), request("session", "result"))
            .await
            .unwrap()
            .linked_now
    );
    assert!(
        conversations
            .append_message_idempotent(id.clone(), request("session", "changed"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_session_deletion_restores_history_and_execution_data() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    let message = persistence
        .conversations()
        .append_message(request("session", "retained"))
        .await
        .unwrap();
    let execution = persistence
        .executions()
        .create(execution("session"))
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON execution_sources BEGIN SELECT RAISE(FAIL, 'injected deletion failure'); END;").unwrap();
    assert!(
        persistence
            .sessions()
            .delete(&SessionId::new("session"))
            .await
            .is_err()
    );
    assert!(
        persistence
            .sessions()
            .get(&SessionId::new("session"))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        persistence
            .messages()
            .get(&message.message.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        persistence
            .executions()
            .get(&execution.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        persistence
            .executions()
            .read_source(&execution.id)
            .await
            .unwrap()
            == b"print hello"
    );
    connection
        .execute_batch("DROP TRIGGER fail_delete")
        .unwrap();
    persistence
        .sessions()
        .delete(&SessionId::new("session"))
        .await
        .unwrap();
    assert!(persistence.messages().list_ids().await.unwrap().is_empty());
    assert!(
        persistence
            .executions()
            .list_all()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn compaction_validation_preserves_the_saved_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    let message = persistence
        .conversations()
        .append_message(request("session", "original"))
        .await
        .unwrap();
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
    persistence.compactions().save(&checkpoint).await.unwrap();
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
        assert!(persistence.compactions().save(&invalid).await.is_err());
    }
    let reopened = open_persistence(directory.path()).await.unwrap();
    assert!(
        reopened
            .compactions()
            .get(&checkpoint.covered_through)
            .await
            .unwrap()
            .as_ref()
            == Some(&checkpoint)
    );
    assert!(
        reopened
            .messages()
            .get(&message.message.id)
            .await
            .unwrap()
            .is_some_and(|message| message.content.text() == Some("original"))
    );
    persistence
        .sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    let before = reopened
        .sessions()
        .observe(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    assert!(
        reopened
            .compactions()
            .delete(&checkpoint.covered_through)
            .await
            .is_err()
    );
    persistence
        .compactions()
        .delete(&checkpoint.covered_through)
        .await
        .unwrap();
    let after = reopened
        .sessions()
        .observe(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    assert!(after.revision == before.revision + 1);
    assert!(
        reopened
            .compactions()
            .get(&checkpoint.covered_through)
            .await
            .unwrap()
            .is_none()
    );
}
