use super::*;
use crate::test_support::test_dir;
use crate::{CompactionCheckpoint, FileCompactionStore, FileMessageStore, init_at};
use kraai_types::{AssistantItem, AssistantPhase, ConversationItem, Message, MessageStatus};
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingMessageStore {
    inner: Arc<dyn MessageStore>,
    reads: AtomicUsize,
}

#[async_trait::async_trait]
impl MessageStore for CountingMessageStore {
    async fn get(&self, id: &MessageId) -> Result<Option<Message>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.get(id).await
    }

    async fn save(&self, message: &Message) -> Result<()> {
        self.inner.save(message).await
    }

    async fn unload(&self, id: &MessageId) {
        self.inner.unload(id).await;
    }

    async fn delete(&self, id: &MessageId) -> Result<()> {
        self.inner.delete(id).await
    }

    async fn exists(&self, id: &MessageId) -> Result<bool> {
        self.inner.exists(id).await
    }

    async fn list_all_on_disk(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_all_on_disk().await
    }

    async fn list_hot(&self) -> Result<HashSet<MessageId>> {
        self.inner.list_hot().await
    }
}

async fn with_test_store<T, F, Fut>(name: &str, f: F) -> T
where
    F: FnOnce(Arc<FileMessageStore>, Arc<FileSessionStore>, PathBuf) -> Fut,
    Fut: Future<Output = T>,
{
    let data_dir = test_dir(name);
    fs::create_dir_all(&data_dir).await.unwrap();
    let message_store = Arc::new(FileMessageStore::new(&data_dir));
    let session_store = Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
    let result = f(message_store, session_store, data_dir.clone()).await;
    let _ = fs::remove_dir_all(&data_dir).await;
    result
}

fn session(id: &str, tip_id: Option<&MessageId>, updated_at: u64) -> SessionMeta {
    SessionMeta {
        id: id.to_string(),
        tip_id: tip_id.cloned(),
        workspace_dir: PathBuf::from("/tmp/workspace"),
        created_at: updated_at.saturating_sub(1),
        updated_at,
        title: Some(format!("session-{id}")),
        selected_profile_id: None,
    }
}

fn message(id: &str, parent_id: Option<&MessageId>, content: &str) -> Message {
    Message {
        id: MessageId::new(id),
        parent_id: parent_id.cloned(),
        content: ConversationItem::Assistant {
            items: vec![AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: content.to_string(),
            }],
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    }
}

#[tokio::test]
async fn cancelled_mutations_publish_sessions_before_releasing_the_write_lock() {
    for operation in ["save", "compare-and-save", "delete"] {
        let data_dir = test_dir(operation);
        let messages = Arc::new(FileMessageStore::new(&data_dir));
        let store = Arc::new(FileSessionStore::new(&data_dir, messages.clone()));
        let root = message("root", None, "root");
        messages.save(&root).await.unwrap();
        store
            .save(&session("session", Some(&root.id), 1))
            .await
            .unwrap();

        let cached = store.state.sessions.read().await;
        let caller = tokio::spawn({
            let store = store.clone();
            let root_id = root.id.clone();
            async move {
                let replacement = session("session", Some(&root_id), 2);
                match operation {
                    "save" => store.save(&replacement).await,
                    "compare-and-save" => {
                        assert!(
                            store
                                .save_if_tip_matches(&replacement, Some(&root_id))
                                .await
                                .unwrap()
                        );
                        Ok(())
                    }
                    _ => store.delete("session").await,
                }
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let bytes = fs::read(&store.state.sessions_path).await.unwrap();
                let persisted: HashMap<String, SessionMeta> =
                    serde_json::from_slice(&bytes).unwrap();
                if persisted.get("session").map(|session| session.updated_at)
                    == if operation == "delete" { None } else { Some(2) }
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(store.write_guard.try_lock().is_err());
        drop(cached);

        let guard =
            tokio::time::timeout(std::time::Duration::from_secs(5), store.write_guard.lock())
                .await
                .unwrap();
        assert_eq!(
            store
                .get("session")
                .await
                .unwrap()
                .map(|session| session.updated_at),
            if operation == "delete" { None } else { Some(2) }
        );
        assert_eq!(
            messages.exists(&root.id).await.unwrap(),
            operation != "delete"
        );
        drop(guard);
        let reopened = FileSessionStore::new(&data_dir, messages);
        reopened.load().await.unwrap();
        assert_eq!(
            reopened.list_ids().await.unwrap(),
            store.list_ids().await.unwrap()
        );
        fs::remove_dir_all(data_dir).await.unwrap();
    }
}

#[tokio::test]
async fn save_failure_does_not_mutate_in_memory_sessions() {
    let data_dir = test_dir("save-failure");
    fs::create_dir_all(&data_dir).await.unwrap();

    let blocking_file = data_dir.join("not-a-directory");
    fs::write(&blocking_file, "x").await.unwrap();

    let message_store = Arc::new(FileMessageStore::new(&data_dir));
    let session_store = FileSessionStore::new(&blocking_file, message_store);

    let err = session_store
        .save(&session("broken", None, 1))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Failed to create directory"));
    assert!(session_store.list().await.unwrap().is_empty());

    let _ = fs::remove_dir_all(&data_dir).await;
}

#[cfg(not(windows))]
#[tokio::test]
async fn replaced_sessions_publish_before_returning_durability_error() {
    with_test_store(
        "session-durability-error",
        |_message_store, session_store, _data_dir| async move {
            let replacement = session("replacement", None, 2);
            let next_sessions = HashMap::from([(replacement.id.clone(), replacement)]);

            let error = session_store
                .state
                .publish_sessions(
                    next_sessions,
                    AtomicWriteOutcome::ReplacedButNotSynced(eyre!("injected parent sync failure")),
                )
                .await
                .unwrap_err();

            assert_eq!(error.to_string(), "injected parent sync failure");
            assert!(session_store.get("replacement").await.unwrap().is_some());
        },
    )
    .await;
}

#[tokio::test]
async fn message_store_rejects_ids_that_could_escape_storage() {
    with_test_store(
        "unsafe-message-id",
        |message_store, _session_store, data_dir| async move {
            for raw in [
                "../sessions",
                "/tmp/kraai-message",
                r"..\sessions",
                "C:escape",
            ] {
                let id = MessageId(Arc::from(raw));
                let unsafe_message = Message {
                    id: id.clone(),
                    parent_id: None,
                    content: ConversationItem::Assistant {
                        items: vec![AssistantItem::Text {
                            phase: AssistantPhase::FinalAnswer,
                            text: String::from("unsafe"),
                        }],
                    },
                    status: MessageStatus::Complete,
                    agent_profile_id: None,
                    generation: None,
                };

                assert!(message_store.save(&unsafe_message).await.is_err());
                assert!(message_store.get(&id).await.is_err());
                assert!(message_store.exists(&id).await.is_err());
                assert!(message_store.delete(&id).await.is_err());
            }

            assert!(!data_dir.join("sessions.json").exists());
        },
    )
    .await;
}

#[tokio::test]
async fn concurrent_save_and_delete_preserve_unrelated_sessions() {
    with_test_store(
        "concurrent-save-delete",
        |message_store, session_store, _| async move {
            let base_message = message("shared-root", None, "root");
            message_store.save(&base_message).await.unwrap();

            session_store
                .save(&session("keep", Some(&base_message.id), 2))
                .await
                .unwrap();
            session_store
                .save(&session("drop", Some(&base_message.id), 1))
                .await
                .unwrap();

            let save_task = {
                let session_store = session_store.clone();
                tokio::spawn(async move {
                    session_store
                        .save(&session("new", Some(&MessageId::new("shared-root")), 3))
                        .await
                        .unwrap();
                })
            };

            let delete_task = {
                let session_store = session_store.clone();
                tokio::spawn(async move {
                    session_store.delete("drop").await.unwrap();
                })
            };

            save_task.await.unwrap();
            delete_task.await.unwrap();

            let ids: HashSet<_> = session_store
                .list()
                .await
                .unwrap()
                .into_iter()
                .map(|session| session.id)
                .collect();

            assert_eq!(
                ids,
                HashSet::from([String::from("keep"), String::from("new")])
            );
        },
    )
    .await;
}

#[tokio::test]
async fn deleting_session_removes_only_unique_messages() {
    with_test_store(
        "delete-unique-messages",
        |message_store, session_store, data_dir| async move {
            let root = message("root", None, "root");
            let shared = message("shared", Some(&root.id), "shared");
            let a_tip = message("a-tip", Some(&shared.id), "a");
            let b_tip = message("b-tip", Some(&shared.id), "b");

            for msg in [&root, &shared, &a_tip, &b_tip] {
                message_store.save(msg).await.unwrap();
            }
            let compactions = FileCompactionStore::new(&data_dir);
            for boundary in [&shared.id, &a_tip.id] {
                compactions
                    .save(&CompactionCheckpoint {
                        covered_through: boundary.clone(),
                        superseded_usage: vec![boundary.clone()],
                        previous_boundary: None,
                        summary: String::from("Completed work"),
                        model_id: kraai_types::ModelId::new("model"),
                        provider_id: kraai_types::ProviderId::new("provider"),
                        prompt_version: 1,
                        usage: None,
                    })
                    .await
                    .unwrap();
            }

            session_store
                .save(&session("a", Some(&a_tip.id), 2))
                .await
                .unwrap();
            session_store
                .save(&session("b", Some(&b_tip.id), 1))
                .await
                .unwrap();

            session_store.delete("a").await.unwrap();

            assert!(!message_store.exists(&a_tip.id).await.unwrap());
            assert!(message_store.exists(&b_tip.id).await.unwrap());
            assert!(message_store.exists(&shared.id).await.unwrap());
            assert!(message_store.exists(&root.id).await.unwrap());
            assert!(compactions.get(&a_tip.id).await.unwrap().is_none());
            assert!(compactions.get(&shared.id).await.unwrap().is_some());
        },
    )
    .await;
}

#[tokio::test]
async fn startup_garbage_collection_leaves_history_out_of_hot_cache() {
    with_test_store(
        "startup-cold-history",
        |message_store, session_store, data_dir| async move {
            let root = message("root", None, "root");
            let tip = message("tip", Some(&root.id), "tip");
            let orphan = message("orphan", None, "orphan");
            for message in [&root, &tip, &orphan] {
                message_store.save(message).await.unwrap();
            }
            session_store
                .save(&session("session", Some(&tip.id), 1))
                .await
                .unwrap();

            let (reopened_messages, _, _, _) = init_at(&data_dir).await.unwrap();

            assert!(reopened_messages.list_hot().await.unwrap().is_empty());
            assert_eq!(
                reopened_messages.list_all_on_disk().await.unwrap(),
                HashSet::from([root.id, tip.id])
            );
        },
    )
    .await;
}

#[tokio::test]
async fn garbage_collection_reads_shared_ancestry_once() {
    with_test_store(
        "shared-history-reads",
        |message_store, _session_store, data_dir| async move {
            let root = message("root", None, "root");
            let shared = message("shared", Some(&root.id), "shared");
            let first_tip = message("first", Some(&shared.id), "first");
            let second_tip = message("second", Some(&shared.id), "second");
            for message in [&root, &shared, &first_tip, &second_tip] {
                message_store.save(message).await.unwrap();
            }
            let messages = Arc::new(CountingMessageStore {
                inner: message_store,
                reads: AtomicUsize::new(0),
            });
            let sessions = FileSessionStore::new(&data_dir, messages.clone());
            sessions
                .save(&session("first", Some(&first_tip.id), 1))
                .await
                .unwrap();
            sessions
                .save(&session("second", Some(&second_tip.id), 2))
                .await
                .unwrap();

            let referenced = sessions
                .state
                .collect_all_referenced_messages()
                .await
                .unwrap();

            assert_eq!(referenced.len(), 4);
            assert_eq!(messages.reads.load(Ordering::Relaxed), 4);
        },
    )
    .await;
}

#[tokio::test]
async fn cyclic_message_graphs_return_corruption_errors() {
    with_test_store(
        "cyclic-message-graphs",
        |message_store, session_store, _| async move {
            let self_cycle_id = MessageId::new("self-cycle");
            message_store
                .save(&message(
                    self_cycle_id.as_str(),
                    Some(&self_cycle_id),
                    "self",
                ))
                .await
                .unwrap();
            let error = session_store
                .state
                .collect_tree_messages(&self_cycle_id)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("self-cycle"));

            let first_id = MessageId::new("cycle-a");
            let second_id = MessageId::new("cycle-b");
            message_store
                .save(&message("cycle-a", Some(&second_id), "a"))
                .await
                .unwrap();
            message_store
                .save(&message("cycle-b", Some(&first_id), "b"))
                .await
                .unwrap();
            let error = session_store
                .state
                .collect_tree_messages(&first_id)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("cycle-a"));
        },
    )
    .await;
}

#[tokio::test]
async fn valid_deep_message_graph_still_traverses() {
    with_test_store(
        "deep-message-graph",
        |message_store, session_store, _| async move {
            let mut parent = None;
            for index in 0..256 {
                let current = message(&format!("deep-{index}"), parent.as_ref(), "item");
                message_store.save(&current).await.unwrap();
                parent = Some(current.id);
            }

            let tree = session_store
                .state
                .collect_tree_messages(parent.as_ref().unwrap())
                .await
                .unwrap();

            assert_eq!(tree.len(), 256);
        },
    )
    .await;
}

#[tokio::test]
async fn list_sorts_sessions_by_updated_at_descending() {
    with_test_store(
        "list-order",
        |_message_store, session_store, _| async move {
            session_store.save(&session("old", None, 1)).await.unwrap();
            session_store.save(&session("new", None, 10)).await.unwrap();
            session_store.save(&session("mid", None, 5)).await.unwrap();

            let ordered_ids: Vec<_> = session_store
                .list()
                .await
                .unwrap()
                .into_iter()
                .map(|session| session.id)
                .collect();

            assert_eq!(ordered_ids, vec!["new", "mid", "old"]);
        },
    )
    .await;
}

#[tokio::test]
async fn delete_surfaces_orphan_cleanup_failures() {
    with_test_store(
        "delete-orphan-failure",
        |message_store, session_store, data_dir| async move {
            let orphan = message("orphan", None, "orphan");
            message_store.save(&orphan).await.unwrap();
            session_store
                .save(&session("drop", Some(&orphan.id), 1))
                .await
                .unwrap();

            let orphan_path = data_dir
                .join("messages")
                .join(format!("{}.json", orphan.id));
            fs::remove_file(&orphan_path).await.unwrap();
            fs::create_dir(&orphan_path).await.unwrap();

            let err = session_store.delete("drop").await.unwrap_err();
            assert!(
                err.to_string()
                    .contains("Failed to delete orphaned messages after session removal")
            );

            fs::remove_dir_all(&orphan_path).await.unwrap();
        },
    )
    .await;
}
