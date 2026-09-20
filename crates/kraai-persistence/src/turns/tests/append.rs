use super::*;

#[tokio::test]
async fn append_message_saves_message_and_advances_session_tip() {
    with_test_store(
        "append-message-advances-tip",
        |message_store, session_store, _| async move {
            session_store
                .save(&untitled_session("session", None, 1))
                .await
                .unwrap();
            let conversation_store =
                ConversationStore::new(message_store.clone(), session_store.clone());

            let appended = conversation_store
                .append_message(append_request(
                    "session",
                    ChatRole::User,
                    "hello",
                    MessageStatus::Complete,
                    Some("hello"),
                ))
                .await
                .unwrap();

            assert_eq!(appended.previous_tip, None);
            let message = message_store
                .get(&appended.message.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(message.parent_id, None);
            assert_eq!(message.role(), ChatRole::User);
            assert_eq!(message.content.text(), Some("hello"));

            let stored_session = session_store.get("session").await.unwrap().unwrap();
            assert_eq!(stored_session.tip_id, Some(appended.message.id));
            assert_eq!(stored_session.title.as_deref(), Some("hello"));
            assert!(stored_session.updated_at >= 1);
        },
    )
    .await;
}

#[tokio::test]
async fn append_message_keeps_existing_title_and_links_to_previous_tip() {
    with_test_store(
        "append-message-existing-title",
        |message_store, session_store, _| async move {
            session_store
                .save(&untitled_session("session", None, 1))
                .await
                .unwrap();
            let conversation_store =
                ConversationStore::new(message_store.clone(), session_store.clone());

            let first = conversation_store
                .append_message(append_request(
                    "session",
                    ChatRole::User,
                    "first",
                    MessageStatus::Complete,
                    Some("first title"),
                ))
                .await
                .unwrap();
            let second = conversation_store
                .append_message(append_request(
                    "session",
                    ChatRole::User,
                    "second",
                    MessageStatus::Complete,
                    Some("second title"),
                ))
                .await
                .unwrap();

            assert_eq!(second.previous_tip, Some(first.message.id.clone()));
            assert_eq!(second.message.parent_id, Some(first.message.id.clone()));
            let stored_session = session_store.get("session").await.unwrap().unwrap();
            assert_eq!(stored_session.tip_id, Some(second.message.id));
            assert_eq!(stored_session.title.as_deref(), Some("first title"));
        },
    )
    .await;
}

#[tokio::test]
async fn append_message_session_save_failure_retains_new_message_for_recovery() {
    let data_dir = test_dir("append-message-save-failure");
    tokio::fs::create_dir_all(&data_dir).await.unwrap();

    let message_store: Arc<dyn MessageStore> = Arc::new(FileMessageStore::new(&data_dir));
    let base_session_store: Arc<dyn SessionStore> =
        Arc::new(FileSessionStore::new(&data_dir, message_store.clone()));
    base_session_store
        .save(&untitled_session("session", None, 1))
        .await
        .unwrap();

    let should_fail = Arc::new(AtomicBool::new(true));
    let failing_session_store: Arc<dyn SessionStore> = Arc::new(FailOnSaveSessionStore {
        inner: base_session_store.clone(),
        should_fail,
        commit_before_failure: false,
    });
    let conversation_store = ConversationStore::new(message_store.clone(), failing_session_store);

    let error = conversation_store
        .append_message(append_request(
            "session",
            ChatRole::User,
            "will roll back",
            MessageStatus::Complete,
            Some("will roll back"),
        ))
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("intentional session save failure")
    );
    assert_eq!(message_store.list_all_on_disk().await.unwrap().len(), 1);
    assert_eq!(
        base_session_store
            .get("session")
            .await
            .unwrap()
            .unwrap()
            .tip_id,
        None
    );

    let _ = tokio::fs::remove_dir_all(&data_dir).await;
}

#[tokio::test]
async fn append_message_preserves_linked_message_after_committed_save_error() {
    with_test_store(
        "append-message-committed-error",
        |message_store, session_store, data_dir| async move {
            session_store
                .save(&untitled_session("session", None, 1))
                .await
                .unwrap();
            let failing_store = Arc::new(FailOnSaveSessionStore {
                inner: session_store.clone(),
                should_fail: Arc::new(AtomicBool::new(true)),
                commit_before_failure: true,
            });
            let conversations = ConversationStore::new(message_store.clone(), failing_store);

            let error = conversations
                .append_message(append_request(
                    "session",
                    ChatRole::User,
                    "committed",
                    MessageStatus::Complete,
                    None,
                ))
                .await
                .unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("intentional session save failure")
            );
            let reopened = FileSessionStore::new(&data_dir, message_store.clone());
            reopened.load().await.unwrap();
            let tip_id = reopened
                .get("session")
                .await
                .unwrap()
                .unwrap()
                .tip_id
                .unwrap();
            assert!(message_store.exists(&tip_id).await.unwrap());
            assert_eq!(
                message_store
                    .get(&tip_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .content
                    .text(),
                Some("committed")
            );
        },
    )
    .await;
}

#[tokio::test]
async fn concurrent_appends_do_not_silently_orphan_a_success() {
    with_test_store(
        "concurrent-appends",
        |message_store, session_store, _| async move {
            session_store
                .save(&untitled_session("session", None, 1))
                .await
                .unwrap();
            let synchronized_messages: Arc<dyn MessageStore> =
                Arc::new(BarrierOnSaveMessageStore {
                    inner: message_store.clone(),
                    barrier: Arc::new(Barrier::new(2)),
                });
            let conversations =
                ConversationStore::new(synchronized_messages, session_store.clone());

            let first = tokio::spawn({
                let conversations = conversations.clone();
                async move {
                    conversations
                        .append_message(append_request(
                            "session",
                            ChatRole::User,
                            "first",
                            MessageStatus::Complete,
                            None,
                        ))
                        .await
                }
            });
            let second = tokio::spawn({
                let conversations = conversations.clone();
                async move {
                    conversations
                        .append_message(append_request(
                            "session",
                            ChatRole::User,
                            "second",
                            MessageStatus::Complete,
                            None,
                        ))
                        .await
                }
            });

            let results = [first.await.unwrap(), second.await.unwrap()];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

            let stored_session = session_store.get("session").await.unwrap().unwrap();
            let tip = stored_session.tip_id.unwrap();
            assert!(message_store.exists(&tip).await.unwrap());
            assert_eq!(message_store.list_all_on_disk().await.unwrap().len(), 1);
        },
    )
    .await;
}
