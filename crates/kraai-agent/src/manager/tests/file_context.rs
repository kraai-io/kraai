use super::super::*;
use super::common::{cleanup_dir, open_persistence, test_manager};
use kraai_persistence::{CompactionCheckpoint, ScriptExecutionStore, SqliteCompactionStore};
use kraai_types::{ContextStateMutation, PinnedFileScope};

async fn open(manager: &AgentManager, session: &SessionId, path: &Path) {
    manager
        .context_state_store
        .append_runtime(
            session,
            "test",
            vec![ContextStateMutation::PinFile {
                path: path.into(),
                scope: PinnedFileScope::Host,
            }],
        )
        .await
        .unwrap();
}

async fn close(manager: &AgentManager, session: &SessionId, path: &Path) {
    manager
        .context_state_store
        .append_runtime(
            session,
            "test",
            vec![ContextStateMutation::UnpinFile {
                path: path.into(),
                reason: None,
            }],
        )
        .await
        .unwrap();
}

async fn request(manager: &AgentManager, session: &SessionId) -> ProviderRequest {
    let tip = manager
        .get_tip(session)
        .await
        .unwrap()
        .expect("missing tip");
    let provider = ProviderId::new("mock");
    let model = ModelId::new("mock-model");
    let history = manager
        .get_model_history(&tip, (&provider, &model), false)
        .await
        .unwrap();
    manager
        .build_model_context(
            session,
            history,
            &prompts::TurnSystemPrompt {
                prefix: "instructions".into(),
                instructions: Vec::new(),
                discovery_warnings: vec![],
                workspace_dir: PathBuf::new(),
            },
            None,
            None,
            &kraai_types::ModelSelection {
                provider_id: provider.clone(),
                model_id: model.clone(),
                options: Default::default(),
            },
        )
        .await
        .unwrap()
        .request
}

fn texts(request: &ProviderRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .map(|item| item.display_text().into_owned())
        .collect()
}

#[tokio::test]
async fn unchanged_files_stay_in_place_and_refresh_only_moves_the_changed_file() {
    let (manager, root) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let a = root.join("a.txt");
    let b = root.join("b.txt");
    tokio::fs::write(&a, "alpha\r\n").await.unwrap();
    tokio::fs::write(&b, "beta\n").await.unwrap();
    manager
        .add_message(&session, ChatRole::User, "first".into(), None)
        .await
        .unwrap();
    open(&manager, &session, &a).await;
    open(&manager, &session, &b).await;
    let first = request(&manager, &session).await;
    assert_eq!(first.messages.len(), 4);
    assert!(texts(&first)[2].contains("1|alpha\r\n"));
    manager
        .add_message(&session, ChatRole::Assistant, "work".into(), None)
        .await
        .unwrap();
    let latest = manager
        .add_message(&session, ChatRole::User, "next".into(), None)
        .await
        .unwrap();
    let unchanged = request(&manager, &session).await;
    assert_eq!(
        &unchanged.messages[..first.messages.len()],
        first.messages.as_slice()
    );
    open(&manager, &session, &a).await;
    assert_eq!(
        request(&manager, &session).await.messages,
        unchanged.messages
    );
    let recorded = serde_json::to_value(manager.get_chat_history(&session).await.unwrap()).unwrap();
    tokio::fs::write(&a, "intermediate\n").await.unwrap();
    tokio::fs::write(&a, "updated\r\n").await.unwrap();
    let changed = request(&manager, &session).await;
    assert_eq!(changed.messages.len(), unchanged.messages.len());
    assert!(texts(&changed)[2].contains("1|beta\n"));
    assert_eq!(texts(&changed)[3], "work");
    assert!(texts(&changed).last().unwrap().contains("1|updated\r\n"));
    assert!(!texts(&changed).join("\n").contains("alpha"));
    let saved = manager
        .context_state_store
        .snapshots(&session)
        .await
        .unwrap();
    assert_eq!(saved.iter().find(|s| s.path == a).unwrap().anchor, latest);
    assert_eq!(
        serde_json::to_value(manager.get_chat_history(&session).await.unwrap()).unwrap(),
        recorded
    );
    close(&manager, &session, &a).await;
    let closed = request(&manager, &session).await;
    assert!(!texts(&closed).join("\n").contains("updated"));
    manager
        .add_message(&session, ChatRole::User, "reopen".into(), None)
        .await
        .unwrap();
    open(&manager, &session, &a).await;
    let reopened = request(&manager, &session).await;
    assert!(texts(&reopened).last().unwrap().contains("updated"));
    assert_eq!(
        reopened
            .messages
            .iter()
            .filter(|m| matches!(m, ConversationItem::FileContext { .. }))
            .count(),
        2
    );
    cleanup_dir(root).await;
}

#[tokio::test]
async fn snapshot_anchors_survive_manager_restart() {
    let (manager, root) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let path = root.join("file.txt");
    tokio::fs::write(&path, "persistent\n").await.unwrap();
    manager
        .add_message(&session, ChatRole::User, "read".into(), None)
        .await
        .unwrap();
    open(&manager, &session, &path).await;
    request(&manager, &session).await;
    manager
        .add_message(&session, ChatRole::User, "later".into(), None)
        .await
        .unwrap();
    let before = request(&manager, &session).await;
    let providers = manager.cloned_provider_manager();
    drop(manager);
    let persistence = open_persistence(&root).await;
    let manager = AgentManager::new(providers, root.clone(), persistence, root.clone());
    assert_eq!(request(&manager, &session).await.messages, before.messages);
    cleanup_dir(root).await;
}

#[tokio::test]
async fn compaction_reanchors_snapshots_once_and_closed_files_do_not_return() {
    let (manager, root) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let path = root.join("file.txt");
    tokio::fs::write(&path, "exact snapshot\n").await.unwrap();
    manager
        .add_message(&session, ChatRole::User, "task".into(), None)
        .await
        .unwrap();
    open(&manager, &session, &path).await;
    request(&manager, &session).await;
    let boundary = manager
        .add_message(&session, ChatRole::Assistant, "done".into(), None)
        .await
        .unwrap();
    let checkpoint = CompactionCheckpoint {
        covered_through: boundary.clone(),
        superseded_usage: vec![],
        previous_boundary: None,
        replacement: vec![
            ConversationItem::User {
                content: "task".into(),
            },
            ConversationItem::Compaction {
                provider_id: ProviderId::new("mock"),
                payload: serde_json::json!({"type":"compaction", "encrypted_content":"opaque"}),
            },
        ],
        provider_id: ProviderId::new("mock"),
        model_id: ModelId::new("mock-model"),
        prompt_version: 1,
        usage: None,
    };
    SqliteCompactionStore::new(&root)
        .save(&checkpoint)
        .await
        .unwrap();
    let compacted = request(&manager, &session).await;
    assert_eq!(compacted.messages.len(), 4);
    assert!(texts(&compacted)[3].contains("exact snapshot"));
    assert_eq!(
        manager
            .context_state_store
            .snapshots(&session)
            .await
            .unwrap()[0]
            .anchor,
        boundary
    );
    manager
        .add_message(&session, ChatRole::User, "continue".into(), None)
        .await
        .unwrap();
    let next = request(&manager, &session).await;
    assert_eq!(&next.messages[..4], compacted.messages.as_slice());
    close(&manager, &session, &path).await;
    assert!(
        !texts(&request(&manager, &session).await)
            .join("\n")
            .contains("exact snapshot")
    );
    cleanup_dir(root).await;
}

#[tokio::test]
async fn unreadable_files_do_not_leave_stale_contents_and_recovery_refreshes_them() {
    let (manager, root) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let path = root.join("file.txt");
    tokio::fs::write(&path, "before\n").await.unwrap();
    manager
        .add_message(&session, ChatRole::User, "read".into(), None)
        .await
        .unwrap();
    open(&manager, &session, &path).await;
    request(&manager, &session).await;
    tokio::fs::write(&path, [0xff]).await.unwrap();
    let unavailable = request(&manager, &session).await;
    assert!(
        texts(&unavailable)
            .join("\n")
            .contains("temporarily unavailable")
    );
    assert!(!texts(&unavailable).join("\n").contains("1|before"));
    manager
        .add_message(&session, ChatRole::User, "continue".into(), None)
        .await
        .unwrap();
    let unchanged_error = request(&manager, &session).await;
    assert_eq!(
        &unchanged_error.messages[..unavailable.messages.len()],
        unavailable.messages.as_slice()
    );
    tokio::fs::write(&path, "recovered\n").await.unwrap();
    assert!(
        texts(&request(&manager, &session).await)
            .last()
            .unwrap()
            .contains("recovered")
    );
    tokio::fs::remove_file(&path).await.unwrap();
    let removed = request(&manager, &session).await;
    assert!(
        texts(&removed)
            .join("\n")
            .contains("automatically unpinned")
    );
    assert!(
        manager
            .context_state_store
            .snapshots(&session)
            .await
            .unwrap()
            .is_empty()
    );
    tokio::fs::write(&path, "replacement\n").await.unwrap();
    assert!(
        !texts(&request(&manager, &session).await)
            .join("\n")
            .contains("replacement")
    );
    cleanup_dir(root).await;
}

#[tokio::test]
async fn abandoned_anchors_move_to_the_current_tip_without_changing_recorded_history() {
    let (manager, root) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let path = root.join("file.txt");
    tokio::fs::write(&path, "contents\n").await.unwrap();
    manager
        .add_message(&session, ChatRole::User, "first".into(), None)
        .await
        .unwrap();
    manager
        .add_message(&session, ChatRole::User, "abandoned".into(), None)
        .await
        .unwrap();
    open(&manager, &session, &path).await;
    request(&manager, &session).await;
    manager.undo_last_user_message(&session).await.unwrap();
    let latest = manager
        .add_message(&session, ChatRole::User, "replacement".into(), None)
        .await
        .unwrap();
    let current = request(&manager, &session).await;
    assert_eq!(texts(&current)[2], "replacement");
    assert!(texts(&current)[3].contains("contents"));
    assert_eq!(
        manager
            .context_state_store
            .snapshots(&session)
            .await
            .unwrap()[0]
            .anchor,
        latest
    );
    cleanup_dir(root).await;
}

#[tokio::test]
async fn refreshing_with_the_same_tip_appends_after_other_snapshots_and_keeps_that_order() {
    let (manager, root) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    manager
        .add_message(&session, ChatRole::User, "task".into(), None)
        .await
        .unwrap();
    let a = root.join("a.txt");
    let b = root.join("b.txt");
    tokio::fs::write(&a, "first file\n").await.unwrap();
    tokio::fs::write(&b, "second file\n").await.unwrap();
    open(&manager, &session, &a).await;
    open(&manager, &session, &b).await;
    request(&manager, &session).await;
    tokio::fs::write(&a, "refreshed first file\n")
        .await
        .unwrap();
    let refreshed = request(&manager, &session).await;
    assert!(texts(&refreshed)[2].contains("second file"));
    assert!(texts(&refreshed)[3].contains("refreshed first file"));
    assert_eq!(
        request(&manager, &session).await.messages,
        refreshed.messages
    );
    manager
        .add_message(&session, ChatRole::User, "continue".into(), None)
        .await
        .unwrap();
    let next = request(&manager, &session).await;
    assert_eq!(&next.messages[..4], refreshed.messages.as_slice());
    cleanup_dir(root).await;
}

async fn open_from_script(
    manager: &AgentManager,
    session: &SessionId,
    source: &MessageId,
    path: &Path,
) {
    let execution = kraai_execution_types::ScriptExecutionId::new(Ulid::generate());
    manager
        .persistence
        .executions()
        .create(kraai_persistence::NewScriptExecution {
            id: execution.clone(),
            session_id: session.clone(),
            source_message_id: source.clone(),
            call_id: ToolCallId::new("call"),
            profile: manager
                .resolve_selected_profile(&manager.require_session(session).await.unwrap())
                .unwrap()
                .snapshot(),
            source: b"open".to_vec(),
            rewrites: Default::default(),
            requested_capabilities: Default::default(),
            effective_capabilities: Default::default(),
            timeout: None,
        })
        .await
        .unwrap();
    manager
        .context_state_store
        .append_command(
            session,
            &execution,
            1,
            &kraai_execution_types::CommandInvocationId::new(Ulid::generate()),
            "kraai-open-files",
            vec![ContextStateMutation::PinFile {
                path: path.into(),
                scope: PinnedFileScope::Host,
            }],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn undo_drops_files_opened_by_the_undone_turn() {
    let (manager, root) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let kept = root.join("kept.txt");
    let undone = root.join("undone.txt");
    tokio::fs::write(&kept, "kept contents\n").await.unwrap();
    tokio::fs::write(&undone, "undone contents\n")
        .await
        .unwrap();
    manager
        .add_message(&session, ChatRole::User, "first".into(), None)
        .await
        .unwrap();
    let first_reply = manager
        .add_message(&session, ChatRole::Assistant, "opening".into(), None)
        .await
        .unwrap();
    open_from_script(&manager, &session, &first_reply, &kept).await;
    manager
        .add_message(&session, ChatRole::User, "second".into(), None)
        .await
        .unwrap();
    let second_reply = manager
        .add_message(&session, ChatRole::Assistant, "opening more".into(), None)
        .await
        .unwrap();
    open_from_script(&manager, &session, &second_reply, &undone).await;
    let before = texts(&request(&manager, &session).await).join("\n");
    assert!(before.contains("undone contents"));
    manager.undo_last_user_message(&session).await.unwrap();
    let after = texts(&request(&manager, &session).await).join("\n");
    assert!(after.contains("kept contents"));
    assert!(!after.contains("undone contents"));
    cleanup_dir(root).await;
}

#[tokio::test]
async fn undo_past_compaction_drops_files_opened_by_the_compacted_turn() {
    let (manager, root) = test_manager().await;
    let session = manager.create_session().await.unwrap();
    let path = root.join("compacted.txt");
    tokio::fs::write(&path, "compacted contents\n")
        .await
        .unwrap();
    manager
        .add_message(&session, ChatRole::User, "first".into(), None)
        .await
        .unwrap();
    let reply = manager
        .add_message(&session, ChatRole::Assistant, "opening".into(), None)
        .await
        .unwrap();
    open_from_script(&manager, &session, &reply, &path).await;
    SqliteCompactionStore::new(&root)
        .save(&CompactionCheckpoint {
            covered_through: reply.clone(),
            superseded_usage: vec![],
            previous_boundary: None,
            replacement: vec![ConversationItem::Compaction {
                provider_id: ProviderId::new("mock"),
                payload: serde_json::json!({"type":"compaction", "encrypted_content":"opaque"}),
            }],
            provider_id: ProviderId::new("mock"),
            model_id: ModelId::new("mock-model"),
            prompt_version: 1,
            usage: None,
        })
        .await
        .unwrap();
    manager
        .add_message(&session, ChatRole::User, "second".into(), None)
        .await
        .unwrap();
    assert!(
        texts(&request(&manager, &session).await)
            .join("\n")
            .contains("compacted contents")
    );
    let document = manager.context_state_store.load(&session).await.unwrap();
    assert!(document.events.is_empty());
    assert_eq!(document.pinned.len(), 1);
    manager.undo_last_user_message(&session).await.unwrap();
    manager.undo_last_user_message(&session).await.unwrap();
    assert!(
        manager
            .context_state_store
            .load(&session)
            .await
            .unwrap()
            .pinned_files()
            .is_empty()
    );
    manager
        .add_message(&session, ChatRole::User, "again".into(), None)
        .await
        .unwrap();
    assert!(
        !texts(&request(&manager, &session).await)
            .join("\n")
            .contains("compacted contents")
    );
    cleanup_dir(root).await;
}
