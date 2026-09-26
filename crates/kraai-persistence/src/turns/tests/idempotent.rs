use super::*;

#[tokio::test]
async fn concurrent_idempotent_appends_share_one_linked_message() {
    for clone_wrapper in [true, false] {
        with_test_store(
            "concurrent-idempotent-appends",
            |message_store, session_store, _| async move {
                session_store
                    .save(&untitled_session("session", None, 1))
                    .await
                    .unwrap();
                let entered = Arc::new(Notify::new());
                let resume = Arc::new(Notify::new());
                let synchronized_messages: Arc<dyn MessageStore> =
                    Arc::new(PauseBeforeFirstSaveMessageStore {
                        inner: message_store.clone(),
                        pause_next_save: AtomicBool::new(true),
                        entered: entered.clone(),
                        resume: resume.clone(),
                    });
                let conversations =
                    ConversationStore::new(synchronized_messages.clone(), session_store.clone());
                let retry_conversations = if clone_wrapper {
                    conversations.clone()
                } else {
                    ConversationStore::new(synchronized_messages, session_store.clone())
                };
                let message_id = MessageId::new(Ulid::generate());
                let request = || {
                    append_request(
                        "session",
                        ChatRole::ToolCallResult,
                        "result",
                        MessageStatus::Complete,
                        None,
                    )
                };
                let mut first = std::pin::pin!(
                    conversations.append_message_idempotent(message_id.clone(), request())
                );
                tokio::select! {
                    result = first.as_mut() => panic!("Append finished before saving: {result:?}"),
                    _ = entered.notified() => {}
                }
                let mut second = std::pin::pin!(
                    retry_conversations.append_message_idempotent(message_id.clone(), request())
                );
                assert!(
                    second
                        .as_mut()
                        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                        .is_pending()
                );
                resume.notify_one();

                let (first, second) = tokio::join!(first, second);
                assert!(first.unwrap().linked_now);
                assert!(!second.unwrap().linked_now);
                assert_eq!(
                    session_store.get("session").await.unwrap().unwrap().tip_id,
                    Some(message_id.clone())
                );
                assert!(message_store.exists(&message_id).await.unwrap());
                assert_eq!(message_store.list_all_on_disk().await.unwrap().len(), 1);
            },
        )
        .await;
    }
}

#[tokio::test]
async fn idempotent_history_search_rejects_parent_cycles() {
    with_test_store(
        "idempotent-history-cycle",
        |message_store, session_store, _| async move {
            let first_id = MessageId::new("cycle-first");
            let second_id = MessageId::new("cycle-second");
            for (id, parent_id) in [
                (first_id.clone(), second_id.clone()),
                (second_id, first_id.clone()),
            ] {
                message_store
                    .save(&Message {
                        id,
                        parent_id: Some(parent_id),
                        content: ConversationItem::User {
                            content: String::from("cycle").into(),
                        },
                        status: MessageStatus::Complete,
                        agent_profile_id: None,
                        generation: None,
                    })
                    .await
                    .unwrap();
            }
            let conversations = ConversationStore::new(message_store, session_store);
            let error = conversations
                .message_is_in_history(Some(first_id), &MessageId::new("missing"))
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("cycle repeats message cycle-first")
            );
        },
    )
    .await;
}

#[tokio::test]
async fn idempotent_append_links_orphan_once_and_recognizes_history() {
    with_test_store(
        "idempotent-append",
        |message_store, session_store, _| async move {
            session_store
                .save(&untitled_session("session", None, 1))
                .await
                .unwrap();
            let conversation_store =
                ConversationStore::new(message_store.clone(), session_store.clone());
            let root = conversation_store
                .append_message(append_request(
                    "session",
                    ChatRole::Assistant,
                    "script",
                    MessageStatus::Complete,
                    None,
                ))
                .await
                .unwrap();
            let result_id = MessageId::new(Ulid::generate());
            message_store
                .save(&Message {
                    id: result_id.clone(),
                    parent_id: Some(root.message.id),
                    content: ConversationItem::ScriptResult {
                        call_id: ToolCallId::new("test-call"),
                        output: String::from("result").into(),
                    },
                    status: MessageStatus::Complete,
                    agent_profile_id: Some(String::from("coding")),
                    generation: None,
                })
                .await
                .unwrap();
            let request = || AppendMessageRequest {
                session_id: String::from("session"),
                content: ConversationItem::ScriptResult {
                    call_id: ToolCallId::new("test-call"),
                    output: String::from("result").into(),
                },
                status: MessageStatus::Complete,
                agent_profile_id: Some(String::from("coding")),
                generation: None,
                title_if_first_message: None,
            };

            let recovered = conversation_store
                .append_message_idempotent(result_id.clone(), request())
                .await
                .unwrap();
            assert!(recovered.linked_now);
            assert_eq!(
                session_store.get("session").await.unwrap().unwrap().tip_id,
                Some(result_id.clone())
            );

            let retry = conversation_store
                .append_message_idempotent(result_id, request())
                .await
                .unwrap();
            assert!(!retry.linked_now);
        },
    )
    .await;
}

#[tokio::test]
async fn idempotent_recovery_rejects_messages_deleted_after_they_were_read() {
    with_test_store(
        "idempotent-recovery-session-delete",
        |message_store, _session_store, data_dir| async move {
            let entered = Arc::new(Notify::new());
            let resume = Arc::new(Notify::new());
            let messages = Arc::new(PauseAfterGetMessageStore {
                inner: message_store.clone(),
                pause_next_get: AtomicBool::new(false),
                entered: entered.clone(),
                resume: resume.clone(),
            });
            let sessions = Arc::new(FileSessionStore::new(&data_dir, messages.clone()));
            sessions
                .save(&untitled_session("session", None, 1))
                .await
                .unwrap();
            let id = MessageId::new("stable-result");
            let request = || {
                append_request(
                    "session",
                    ChatRole::ToolCallResult,
                    "result",
                    MessageStatus::Complete,
                    None,
                )
            };
            message_store
                .save(&Message {
                    id: id.clone(),
                    parent_id: None,
                    content: request().content,
                    status: MessageStatus::Complete,
                    agent_profile_id: None,
                    generation: None,
                })
                .await
                .unwrap();
            sessions
                .save(&untitled_session("other", Some(&id), 1))
                .await
                .unwrap();
            messages.pause_next_get.store(true, Ordering::SeqCst);
            let conversations = ConversationStore::new(messages, sessions.clone());
            let recovery = tokio::spawn({
                let conversations = conversations.clone();
                let id = id.clone();
                let request = request();
                async move { conversations.append_message_idempotent(id, request).await }
            });
            entered.notified().await;
            sessions.delete("other").await.unwrap();
            resume.notify_one();

            let error = recovery.await.unwrap().unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Cannot link missing message stable-result")
            );
            assert!(
                sessions
                    .get("session")
                    .await
                    .unwrap()
                    .unwrap()
                    .tip_id
                    .is_none()
            );
            assert!(!message_store.exists(&id).await.unwrap());
            assert!(
                conversations
                    .append_message_idempotent(id.clone(), request())
                    .await
                    .unwrap()
                    .linked_now
            );
            assert_eq!(
                sessions.get("session").await.unwrap().unwrap().tip_id,
                Some(id.clone())
            );
            assert!(message_store.exists(&id).await.unwrap());
        },
    )
    .await;
}
