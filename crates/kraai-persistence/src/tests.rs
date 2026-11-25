use crate::*;
use kraai_execution_types::{
    SandboxCapabilities, ScriptExecutionId, ScriptExecutionPhase, ScriptExecutionStatus,
    ScriptOutputStream, ScriptProfileSnapshot, ScriptRewrites,
};
use kraai_types::{
    ContextStateMutation, ConversationItem, MessageId, MessageStatus, PinnedFileScope, SessionId,
    ToolCallId,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

async fn open_persistence(data_dir: &Path) -> Result<Persistence> {
    Persistence::open_with_turn_lease(data_dir, kraai_test_support::OUTLIVES_ANY_TEST).await
}

fn session(id: &str) -> SessionMeta {
    SessionMeta {
        revision: 0,
        id: SessionId::new(id),
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
        session_id: SessionId::new(session),
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
        session_id: SessionId::new(session),
        source_message_id: MessageId::new("source"),
        call_id: ToolCallId::new("call"),
        profile: ScriptProfileSnapshot {
            id: "test".into(),
            permissions: kraai_execution_types::SandboxPermissionSet::new([
                kraai_execution_types::SandboxCapability::WorkspaceRead,
            ]),
            permission_rules: Default::default(),
            commands: Vec::new(),
            escalation_policy: kraai_execution_types::EscalationPolicy::Deny,
            environment: kraai_execution_types::EnvironmentPolicy::Minimal,
            nushell_startup: kraai_execution_types::NushellStartup::Clean,
            path: kraai_execution_types::PathPolicy::Inherit,
        },
        source: b"print hello".to_vec(),
        rewrites: ScriptRewrites::default(),
        requested_capabilities: SandboxCapabilities::default(),
        effective_capabilities: SandboxCapabilities::default(),
        timeout: Some(Duration::from_secs(30)),
    }
}

#[tokio::test]
async fn independent_clients_keep_changes_to_different_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    let first_session = session("a");
    let second_session = session("b");
    let (first, second) = tokio::join!(
        a.sessions().save(&first_session),
        b.sessions().save(&second_session)
    );
    first.unwrap();
    second.unwrap();
    assert!(a.sessions().list().await.unwrap().len() == 2);
    assert!(b.sessions().list().await.unwrap().len() == 2);
    let first_conversation = a.conversations();
    let second_conversation = b.conversations();
    let (first, second) = tokio::join!(
        first_conversation.append_message(request("a", "first")),
        second_conversation.append_message(request("b", "second"))
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert!(first.message.content.text() == Some("first"));
    assert!(second.message.content.text() == Some("second"));
}

#[tokio::test]
async fn only_one_simultaneous_claim_succeeds() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    let id = SessionId::new("session");
    let (first, second) = tokio::join!(a.sessions().claim_turn(&id), b.sessions().claim_turn(&id));
    assert!(first.is_ok() != second.is_ok());
    let (owner, observer) = if first.is_ok() { (&a, &b) } else { (&b, &a) };
    owner
        .conversations()
        .append_message(request("session", "accepted"))
        .await
        .unwrap();
    assert!(
        observer
            .conversations()
            .append_message(request("session", "rejected"))
            .await
            .is_err()
    );
    assert!(
        observer
            .sessions()
            .delete(&SessionId::new("session"))
            .await
            .is_err()
    );
    assert!(observer.messages().list_ids().await.unwrap().len() == 1);
}

#[tokio::test]
async fn expired_owner_cannot_write_renew_or_release_a_new_claim() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    a.sessions()
        .claim_turn_for(&SessionId::new("session"), Duration::from_millis(1))
        .await
        .unwrap();
    let old = a
        .conversations()
        .append_message(request("session", "old"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let b = open_persistence(directory.path()).await.unwrap();
    b.sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    assert!(a.messages().save(&old.message).await.is_err());
    assert!(
        a.sessions()
            .renew_turn(&SessionId::new("session"))
            .await
            .is_err()
    );
    a.sessions()
        .release_turn(&SessionId::new("session"))
        .await
        .unwrap();
    assert!(
        b.sessions()
            .owns_turn(&SessionId::new("session"))
            .await
            .unwrap()
    );
    b.conversations()
        .append_message(request("session", "new"))
        .await
        .unwrap();
}

#[tokio::test]
async fn release_allows_immediate_submission_without_reusing_the_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    a.sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    let before = a
        .sessions()
        .observe(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    a.sessions()
        .release_turn(&SessionId::new("session"))
        .await
        .unwrap();
    b.sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    let after = b
        .sessions()
        .observe(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    assert!(after.lease_expires_at > before.lease_expires_at);
}

#[tokio::test]
async fn renewal_changes_the_token_and_prevents_takeover() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    a.sessions()
        .claim_turn_for(&SessionId::new("session"), Duration::from_millis(1))
        .await
        .unwrap();
    a.sessions()
        .renew_turn(&SessionId::new("session"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let b = open_persistence(directory.path()).await.unwrap();
    assert!(
        b.sessions()
            .claim_turn(&SessionId::new("session"))
            .await
            .is_err()
    );
    a.conversations()
        .append_message(request("session", "still owned"))
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_metadata_is_rejected_instead_of_overwriting_another_client() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    let mut stale = b
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    a.conversations()
        .append_message(request("session", "new message"))
        .await
        .unwrap();
    stale.title = Some("stale title".into());
    assert!(b.sessions().save(&stale).await.is_err());
}

#[tokio::test]
async fn opening_an_observer_does_not_delete_unlinked_messages() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
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
    a.messages().save(&message).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    assert!(b.messages().get(&message.id).await.unwrap().is_some());
}

#[tokio::test]
async fn rollback_preserves_shared_history_and_deletion_removes_associated_state() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("a")).await.unwrap();
    let root = a
        .conversations()
        .append_message(request("a", "root"))
        .await
        .unwrap();
    let mut shared = session("b");
    shared.tip_id = Some(root.message.id.clone());
    a.sessions().save(&shared).await.unwrap();
    let abandoned = a
        .conversations()
        .append_message(request("a", "abandoned"))
        .await
        .unwrap();
    a.conversations()
        .restore_appended_message(&SessionId::new("a"), &abandoned)
        .await
        .unwrap();
    assert!(
        a.messages()
            .get(&abandoned.message.id)
            .await
            .unwrap()
            .is_none()
    );
    a.sessions().delete(&SessionId::new("a")).await.unwrap();
    assert!(a.messages().get(&root.message.id).await.unwrap().is_some());
    a.sessions().delete(&SessionId::new("b")).await.unwrap();
    assert!(a.messages().get(&root.message.id).await.unwrap().is_none());
}

#[tokio::test]
async fn context_mutations_are_serialized_across_connections() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    let mutation = |name: &str| {
        vec![ContextStateMutation::PinFile {
            path: PathBuf::from(format!("/workspace/{name}")),
            scope: PinnedFileScope::Workspace {
                root: PathBuf::from("/workspace"),
            },
        }]
    };
    let id = SessionId::new("session");
    let (first, second) = tokio::join!(
        a.context().append_runtime(&id, "test", mutation("a")),
        b.context().append_runtime(&id, "test", mutation("b"))
    );
    first.unwrap();
    second.unwrap();
    assert!(
        a.context()
            .load(&SessionId::new("session"))
            .await
            .unwrap()
            .events
            .len()
            == 2
    );
}

#[tokio::test]
async fn expired_script_lease_allows_takeover_and_rejects_old_execution_writes() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    a.sessions()
        .claim_turn_for(&SessionId::new("session"), Duration::from_millis(1))
        .await
        .unwrap();
    let record = a.executions().create(execution("session")).await.unwrap();
    a.executions().mark_running(&record.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let b = open_persistence(directory.path()).await.unwrap();
    b.sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    assert!(
        a.sessions()
            .renew_turn(&SessionId::new("session"))
            .await
            .is_err()
    );
    assert!(
        a.executions()
            .append_output(
                &record.id,
                vec![(ScriptOutputStream::Stdout, b"stale".to_vec())]
            )
            .await
            .is_err()
    );
    a.sessions()
        .release_turn(&SessionId::new("session"))
        .await
        .unwrap();
    assert!(
        b.sessions()
            .owns_turn(&SessionId::new("session"))
            .await
            .unwrap()
    );
    assert!(
        b.executions()
            .get(&record.id)
            .await
            .unwrap()
            .is_some_and(|record| record.phase == ScriptExecutionPhase::Running)
    );
}

#[tokio::test]
async fn execution_sources_output_and_terminal_outcomes_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
    let record = a.executions().create(execution("session")).await.unwrap();
    a.executions().mark_running(&record.id).await.unwrap();
    a.executions()
        .append_output(
            &record.id,
            vec![(ScriptOutputStream::Stdout, b"prefix".to_vec())],
        )
        .await
        .unwrap();
    a.executions()
        .append_output(
            &record.id,
            vec![(ScriptOutputStream::Stderr, b"error".to_vec())],
        )
        .await
        .unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    assert!(b.executions().read_output(&record.id).await.unwrap().stdout == b"prefix");
    a.executions()
        .finish(
            &record.id,
            ScriptExecutionCompletion {
                status: ScriptExecutionStatus::Completed,
                exit_code: Some(0),
                sandbox_denied: false,
                sandbox_hint: None,
                error: None,
                stdout: b"complete\0binary".to_vec(),
                stderr: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(
        b.executions()
            .get(&record.id)
            .await
            .unwrap()
            .is_some_and(|record| record.status == Some(ScriptExecutionStatus::Completed))
    );
    assert!(b.executions().read_source(&record.id).await.unwrap() == b"print hello");
    assert!(b.executions().read_output(&record.id).await.unwrap().stdout == b"complete\0binary");
    assert!(
        b.executions()
            .read_output(&record.id)
            .await
            .unwrap()
            .stderr
            .is_empty()
    );
    assert!(b.executions().mark_running(&record.id).await.is_err());
}

#[tokio::test]
async fn cancelled_waiter_does_not_interrupt_a_database_commit() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("session")).await.unwrap();
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
    entered_rx.await.unwrap();
    caller.abort();
    release_tx.send(()).unwrap();
    let _ = caller.await;
    a.sessions().database.run(|_, _| Ok(())).await.unwrap();
    assert!(
        a.sessions()
            .observe(&SessionId::new("session"))
            .await
            .unwrap()
            .is_some_and(|session| session.revision == 2)
    );
}

mod shared_references;
mod storage;
mod transactions;
