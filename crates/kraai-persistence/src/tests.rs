use crate::*;
use color_eyre::eyre::{Result, ensure};
use kraai_types::{
    ContextStateMutation, ConversationItem, MessageId, MessageStatus, PinnedFileScope,
    SandboxCapabilities, ScriptExecutionId, ScriptExecutionPhase, ScriptExecutionStatus,
    ScriptOutputStream, ScriptProfileSnapshot, ToolCallId,
};
use std::path::PathBuf;
use std::time::Duration;

fn session(id: &str) -> SessionMeta {
    SessionMeta {
        revision: 0,
        id: id.into(),
        tip_id: None,
        workspace_dir: PathBuf::from("/workspace"),
        created_at: 1,
        updated_at: 1,
        title: None,
        selected_profile_id: None,
        selected_model: None,
    }
}
fn request(session: &str, text: &str) -> AppendMessageRequest {
    AppendMessageRequest {
        session_id: session.into(),
        content: ConversationItem::User {
            content: text.into(),
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
        title_if_first_message: Some(text.into()),
    }
}
fn execution(session: &str) -> NewScriptExecution {
    NewScriptExecution {
        id: ScriptExecutionId::new("execution"),
        session_id: session.into(),
        source_message_id: MessageId::new("source"),
        call_id: ToolCallId::new("call"),
        profile: ScriptProfileSnapshot {
            id: "test".into(),
            permissions: kraai_types::SandboxPermissionSet::workspace_read(),
            permission_rules: Default::default(),
            commands: Vec::new(),
            escalation_policy: kraai_types::EscalationPolicy::Deny,
            environment: kraai_types::EnvironmentPolicy::Minimal,
            nushell_startup: kraai_types::NushellStartup::Clean,
            path: kraai_types::PathPolicy::Inherit,
        },
        source: b"print hello".to_vec(),
        requested_capabilities: SandboxCapabilities::default(),
        effective_capabilities: SandboxCapabilities::default(),
        timeout: Some(Duration::from_secs(30)),
    }
}

#[tokio::test]
async fn independent_clients_keep_changes_to_different_sessions() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    let b = Persistence::open(directory.path()).await?;
    let first_session = session("a");
    let second_session = session("b");
    let (first, second) = tokio::join!(
        a.sessions().save(&first_session),
        b.sessions().save(&second_session)
    );
    first?;
    second?;
    ensure!(a.sessions().list().await?.len() == 2);
    ensure!(b.sessions().list().await?.len() == 2);
    let first_conversation = a.conversations();
    let second_conversation = b.conversations();
    let (first, second) = tokio::join!(
        first_conversation.append_message(request("a", "first")),
        second_conversation.append_message(request("b", "second"))
    );
    let first = first?;
    let second = second?;
    ensure!(first.message.content.text() == Some("first"));
    ensure!(second.message.content.text() == Some("second"));
    Ok(())
}

#[tokio::test]
async fn only_one_simultaneous_claim_succeeds() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    let b = Persistence::open(directory.path()).await?;
    let (first, second) = tokio::join!(
        a.sessions().claim_turn("session"),
        b.sessions().claim_turn("session")
    );
    ensure!(first.is_ok() != second.is_ok());
    let (owner, observer) = if first.is_ok() { (&a, &b) } else { (&b, &a) };
    owner
        .conversations()
        .append_message(request("session", "accepted"))
        .await?;
    ensure!(
        observer
            .conversations()
            .append_message(request("session", "rejected"))
            .await
            .is_err()
    );
    ensure!(observer.sessions().delete("session").await.is_err());
    ensure!(observer.messages().list_ids().await?.len() == 1);
    Ok(())
}

#[tokio::test]
async fn expired_owner_cannot_write_renew_or_release_a_new_claim() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    a.sessions()
        .claim_turn_for("session", Duration::from_millis(1))
        .await?;
    let old = a
        .conversations()
        .append_message(request("session", "old"))
        .await?;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let b = Persistence::open(directory.path()).await?;
    b.sessions().claim_turn("session").await?;
    ensure!(a.messages().save(&old.message).await.is_err());
    ensure!(a.sessions().renew_turn("session").await.is_err());
    a.sessions().release_turn("session").await?;
    ensure!(b.sessions().owns_turn("session").await?);
    b.conversations()
        .append_message(request("session", "new"))
        .await?;
    Ok(())
}

#[tokio::test]
async fn release_allows_immediate_submission_without_reusing_the_expiry() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    let b = Persistence::open(directory.path()).await?;
    a.sessions().claim_turn("session").await?;
    let before = a
        .sessions()
        .observe("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    a.sessions().release_turn("session").await?;
    b.sessions().claim_turn("session").await?;
    let after = b
        .sessions()
        .observe("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    ensure!(after.lease_expires_at > before.lease_expires_at);
    Ok(())
}

#[tokio::test]
async fn renewal_changes_the_token_and_prevents_takeover() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    a.sessions()
        .claim_turn_for("session", Duration::from_millis(1))
        .await?;
    a.sessions().renew_turn("session").await?;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let b = Persistence::open(directory.path()).await?;
    ensure!(b.sessions().claim_turn("session").await.is_err());
    a.conversations()
        .append_message(request("session", "still owned"))
        .await?;
    Ok(())
}

#[tokio::test]
async fn stale_metadata_is_rejected_instead_of_overwriting_another_client() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    let b = Persistence::open(directory.path()).await?;
    let mut stale = b
        .sessions()
        .get("session")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    a.conversations()
        .append_message(request("session", "new message"))
        .await?;
    stale.title = Some("stale title".into());
    ensure!(b.sessions().save(&stale).await.is_err());
    Ok(())
}

#[tokio::test]
async fn legacy_json_and_deletion_markers_are_ignored() -> Result<()> {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("sessions.json"),
        b"invalid legacy JSON",
    )?;
    std::fs::create_dir(directory.path().join("session-deletions"))?;
    std::fs::write(
        directory.path().join("session-deletions/session.json"),
        b"invalid marker",
    )?;
    let persistence = Persistence::open(directory.path()).await?;
    ensure!(persistence.sessions().list().await?.is_empty());
    ensure!(std::fs::read(directory.path().join("sessions.json"))? == b"invalid legacy JSON");
    Ok(())
}

#[tokio::test]
async fn opening_an_observer_does_not_delete_unlinked_messages() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    let message = kraai_types::Message {
        id: MessageId::new("unlinked"),
        parent_id: None,
        content: ConversationItem::User {
            content: "pending".into(),
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    };
    a.messages().save(&message).await?;
    let b = Persistence::open(directory.path()).await?;
    ensure!(b.messages().get(&message.id).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn rollback_preserves_shared_history_and_deletion_removes_associated_state() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("a")).await?;
    let root = a
        .conversations()
        .append_message(request("a", "root"))
        .await?;
    let mut shared = session("b");
    shared.tip_id = Some(root.message.id.clone());
    a.sessions().save(&shared).await?;
    let abandoned = a
        .conversations()
        .append_message(request("a", "abandoned"))
        .await?;
    a.conversations()
        .restore_appended_message("a", &abandoned)
        .await?;
    ensure!(a.messages().get(&abandoned.message.id).await?.is_none());
    a.sessions().delete("a").await?;
    ensure!(a.messages().get(&root.message.id).await?.is_some());
    a.sessions().delete("b").await?;
    ensure!(a.messages().get(&root.message.id).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn context_mutations_are_serialized_across_connections() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    let b = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    let mutation = |name: &str| {
        vec![ContextStateMutation::PinFile {
            path: PathBuf::from(format!("/workspace/{name}")),
            scope: PinnedFileScope::Workspace {
                root: PathBuf::from("/workspace"),
            },
        }]
    };
    let (first, second) = tokio::join!(
        a.context().append_runtime("session", "test", mutation("a")),
        b.context().append_runtime("session", "test", mutation("b"))
    );
    first?;
    second?;
    ensure!(a.context().load("session").await?.events.len() == 2);
    Ok(())
}

#[tokio::test]
async fn expired_script_lease_allows_takeover_and_rejects_old_execution_writes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    a.sessions()
        .claim_turn_for("session", Duration::from_millis(1))
        .await?;
    let record = a.executions().create(execution("session")).await?;
    a.executions().mark_running(&record.id).await?;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let b = Persistence::open(directory.path()).await?;
    b.sessions().claim_turn("session").await?;
    ensure!(a.sessions().renew_turn("session").await.is_err());
    ensure!(
        a.executions()
            .append_output(&record.id, ScriptOutputStream::Stdout, b"stale".to_vec())
            .await
            .is_err()
    );
    a.sessions().release_turn("session").await?;
    ensure!(b.sessions().owns_turn("session").await?);
    ensure!(
        b.executions()
            .get(&record.id)
            .await?
            .is_some_and(|record| record.phase == ScriptExecutionPhase::Running)
    );
    Ok(())
}

#[tokio::test]
async fn execution_sources_output_and_terminal_outcomes_survive_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    let record = a.executions().create(execution("session")).await?;
    a.executions().mark_running(&record.id).await?;
    a.executions()
        .append_output(&record.id, ScriptOutputStream::Stdout, b"prefix".to_vec())
        .await?;
    a.executions()
        .append_output(&record.id, ScriptOutputStream::Stderr, b"error".to_vec())
        .await?;
    let b = Persistence::open(directory.path()).await?;
    ensure!(b.executions().read_output(&record.id).await?.stdout == b"prefix");
    a.executions()
        .finish(
            &record.id,
            ScriptExecutionCompletion {
                status: ScriptExecutionStatus::Completed,
                exit_code: Some(0),
                sandbox_denied: false,
                error: None,
                stdout: b"complete".to_vec(),
                stderr: Vec::new(),
            },
        )
        .await?;
    ensure!(b.executions().read_source(&record.id).await? == b"print hello");
    ensure!(b.executions().read_output(&record.id).await?.stdout == b"complete");
    ensure!(
        b.executions()
            .read_output(&record.id)
            .await?
            .stderr
            .is_empty()
    );
    ensure!(b.executions().mark_running(&record.id).await.is_err());
    Ok(())
}

#[tokio::test]
async fn cancelled_waiter_does_not_interrupt_a_database_commit() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("session")).await?;
    let database = a.sessions().database.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let caller = tokio::spawn(async move {
        database
            .transaction(move |transaction, _| {
                transaction.execute(
                    "UPDATE sessions SET revision = revision + 1 WHERE id = 'session'",
                    [],
                )?;
                let _ = entered_tx.send(());
                release_rx.recv()?;
                Ok(())
            })
            .await
    });
    entered_rx.await?;
    caller.abort();
    release_tx.send(())?;
    let _ = caller.await;
    ensure!(
        a.sessions()
            .observe("session")
            .await?
            .is_some_and(|session| session.revision == 2)
    );
    Ok(())
}

mod transactions;
