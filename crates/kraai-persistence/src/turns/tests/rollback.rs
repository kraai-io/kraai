use super::*;

#[tokio::test]
async fn append_and_rollback_race_cannot_restore_over_newer_tip() {
    with_test_store(
        "append-rollback-race",
        |_message_store, session_store, _| async move {
            let abandoned_id = MessageId::new("abandoned");
            let newer_id = MessageId::new("newer");
            let original = untitled_session("session", Some(&abandoned_id), 1);
            session_store.save(&original).await.unwrap();

            let mut append_update = original.clone();
            append_update.tip_id = Some(newer_id.clone());
            let mut stale_rollback = original;
            stale_rollback.tip_id = None;

            assert!(
                session_store
                    .save_if_tip_matches(&append_update, Some(&abandoned_id))
                    .await
                    .unwrap()
            );
            assert!(
                !session_store
                    .save_if_tip_matches(&stale_rollback, Some(&abandoned_id))
                    .await
                    .unwrap()
            );
            let stored_session = session_store.get("session").await.unwrap().unwrap();
            assert_eq!(stored_session.tip_id, Some(newer_id));
        },
    )
    .await;
}

#[tokio::test]
async fn restore_tip_title_and_delete_message_requires_abandoned_message_to_be_tip() {
    with_test_store(
        "restore-tip-guard",
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
                    ChatRole::User,
                    "root",
                    MessageStatus::Complete,
                    Some("root title"),
                ))
                .await
                .unwrap();
            let streaming = conversation_store
                .append_message(append_request(
                    "session",
                    ChatRole::Assistant,
                    "",
                    MessageStatus::Streaming {
                        stream_id: kraai_types::StreamId::new(Ulid::generate()),
                    },
                    None,
                ))
                .await
                .unwrap();

            let error = conversation_store
                .restore_tip_title_and_delete_message(
                    "session",
                    &root.message.id,
                    root.previous_tip.clone(),
                    root.previous_title.clone(),
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("tip is not abandoned message"));
            assert!(message_store.exists(&root.message.id).await.unwrap());

            conversation_store
                .restore_appended_message("session", &streaming)
                .await
                .unwrap();

            let stored_session = session_store.get("session").await.unwrap().unwrap();
            assert_eq!(stored_session.tip_id, Some(root.message.id.clone()));
            assert_eq!(stored_session.title.as_deref(), Some("root title"));
            assert!(!message_store.exists(&streaming.message.id).await.unwrap());
            assert!(message_store.exists(&root.message.id).await.unwrap());
        },
    )
    .await;
}

#[tokio::test]
async fn restore_tip_title_succeeds_when_abandoned_message_cleanup_fails() {
    with_test_store(
        "restore-tip-delete-failure",
        |message_store, session_store, _| async move {
            session_store
                .save(&untitled_session("session", None, 1))
                .await
                .unwrap();
            let should_fail = Arc::new(AtomicBool::new(true));
            let failing_message_store: Arc<dyn MessageStore> = Arc::new(FailOnDeleteMessageStore {
                inner: message_store.clone(),
                should_fail,
            });
            let conversation_store =
                ConversationStore::new(failing_message_store, session_store.clone());
            let root = conversation_store
                .append_message(append_request(
                    "session",
                    ChatRole::User,
                    "root",
                    MessageStatus::Complete,
                    Some("root title"),
                ))
                .await
                .unwrap();
            let streaming = conversation_store
                .append_message(append_request(
                    "session",
                    ChatRole::Assistant,
                    "",
                    MessageStatus::Streaming {
                        stream_id: kraai_types::StreamId::new(Ulid::generate()),
                    },
                    None,
                ))
                .await
                .unwrap();

            conversation_store
                .restore_appended_message("session", &streaming)
                .await
                .unwrap();

            let stored_session = session_store.get("session").await.unwrap().unwrap();
            assert_eq!(stored_session.tip_id, Some(root.message.id));
            assert_eq!(stored_session.title.as_deref(), Some("root title"));
            assert!(message_store.exists(&streaming.message.id).await.unwrap());
        },
    )
    .await;
}

#[tokio::test]
async fn rollback_preserves_messages_referenced_by_other_sessions() {
    for shared_is_tip in [true, false] {
        with_test_store(
            "rollback-shared-message",
            |message_store, session_store, _| async move {
                session_store
                    .save(&untitled_session("session", None, 1))
                    .await
                    .unwrap();
                let conversations =
                    ConversationStore::new(message_store.clone(), session_store.clone());
                let appended = conversations
                    .append_message(append_request(
                        "session",
                        ChatRole::User,
                        "shared",
                        MessageStatus::Complete,
                        None,
                    ))
                    .await
                    .unwrap();
                let other_tip = if shared_is_tip {
                    appended.message.id.clone()
                } else {
                    let descendant = Message {
                        id: MessageId::new("descendant"),
                        parent_id: Some(appended.message.id.clone()),
                        ..appended.message.clone()
                    };
                    message_store.save(&descendant).await.unwrap();
                    descendant.id
                };
                session_store
                    .save(&untitled_session("other", Some(&other_tip), 2))
                    .await
                    .unwrap();

                conversations
                    .restore_appended_message("session", &appended)
                    .await
                    .unwrap();

                assert!(
                    session_store
                        .get("session")
                        .await
                        .unwrap()
                        .unwrap()
                        .tip_id
                        .is_none()
                );
                assert_eq!(
                    session_store.get("other").await.unwrap().unwrap().tip_id,
                    Some(other_tip)
                );
                let saved = message_store
                    .get(&appended.message.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(saved.parent_id, appended.message.parent_id);
                assert_eq!(saved.content, appended.message.content);
            },
        )
        .await;
    }
}

#[tokio::test]
async fn rollback_cleanup_excludes_retries_even_if_the_caller_is_cancelled() {
    for cancel_caller in [false, true] {
        with_test_store(
            "rollback-retry",
            |message_store, _session_store, data_dir| async move {
                let entered = Arc::new(Notify::new());
                let resume = Arc::new(Notify::new());
                let messages: Arc<dyn MessageStore> = Arc::new(PauseBeforeDeleteMessageStore {
                    inner: message_store.clone(),
                    entered: entered.clone(),
                    resume: resume.clone(),
                });
                let sessions = Arc::new(FileSessionStore::new(&data_dir, messages.clone()));
                sessions
                    .save(&untitled_session("session", None, 1))
                    .await
                    .unwrap();
                let conversations = ConversationStore::new(messages.clone(), sessions.clone());
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
                conversations
                    .append_message_idempotent(id.clone(), request())
                    .await
                    .unwrap();
                let rollback = tokio::spawn({
                    let conversations = conversations.clone();
                    let id = id.clone();
                    async move {
                        conversations
                            .restore_tip_title_and_delete_message("session", &id, None, None)
                            .await
                    }
                });
                entered.notified().await;
                let rollback = if cancel_caller {
                    rollback.abort();
                    assert!(rollback.await.unwrap_err().is_cancelled());
                    None
                } else {
                    Some(rollback)
                };
                {
                    let mut locked = std::pin::pin!(sessions.lock_message_mutation(&id));
                    assert!(
                        locked
                            .as_mut()
                            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                            .is_pending()
                    );
                }

                let retry = tokio::spawn({
                    let conversations = ConversationStore::new(messages, sessions.clone());
                    let id = id.clone();
                    let request = request();
                    async move { conversations.append_message_idempotent(id, request).await }
                });
                resume.notify_one();
                if let Some(rollback) = rollback {
                    rollback.await.unwrap().unwrap();
                }
                assert!(retry.await.unwrap().unwrap().linked_now);
                assert_eq!(
                    sessions.get("session").await.unwrap().unwrap().tip_id,
                    Some(id.clone())
                );
                assert!(message_store.get(&id).await.unwrap().is_some());
            },
        )
        .await;
    }
}
