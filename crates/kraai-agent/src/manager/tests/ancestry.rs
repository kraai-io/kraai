use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn source_ancestry_queries_stop_after_finding_all_candidates() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let older = manager
        .add_message(&session_id, ChatRole::User, "older".into(), None)
        .await
        .unwrap();
    let first = manager
        .add_message(&session_id, ChatRole::Assistant, "first".into(), None)
        .await
        .unwrap();
    let second = manager
        .add_message(&session_id, ChatRole::Assistant, "second".into(), None)
        .await
        .unwrap();
    let _tip = manager
        .add_message(&session_id, ChatRole::User, "tip".into(), None)
        .await
        .unwrap();
    super::common::corrupt_message(&data_dir, &older, "{}");

    let candidates = HashSet::from([first, second]);
    assert_eq!(
        manager
            .reachable_message_ids(&session_id, candidates.clone())
            .await
            .unwrap(),
        candidates
    );
    let error = manager
        .reachable_message_ids(&session_id, HashSet::from([MessageId::new("missing")]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing field"));
    cleanup_dir(data_dir).await;
}

#[tokio::test]
async fn source_ancestry_queries_distinguish_roots_missing_messages_and_cycles() {
    let (manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await.unwrap();
    let missing = MessageId::new("missing");
    let root = manager
        .add_message(&session_id, ChatRole::User, "root".into(), None)
        .await
        .unwrap();
    assert!(
        manager
            .reachable_message_ids(&session_id, HashSet::from([missing.clone()]))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        manager
            .reachable_message_ids(&session_id, HashSet::from([root.clone()]))
            .await
            .unwrap(),
        HashSet::from([root.clone()])
    );

    manager
        .set_tip(&session_id, Some(missing.clone()))
        .await
        .unwrap();
    let error = manager
        .reachable_message_ids(&session_id, HashSet::from([missing.clone()]))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Message missing disappeared while reading session history"
    );

    let mut message = manager.message_store.get(&root).await.unwrap().unwrap();
    message.parent_id = Some(root.clone());
    manager.message_store.save(&message).await.unwrap();
    manager
        .set_tip(&session_id, Some(root.clone()))
        .await
        .unwrap();
    let error = manager
        .reachable_message_ids(&session_id, HashSet::from([missing]))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("Corrupt message parent graph: cycle repeats message {root}")
    );
    cleanup_dir(data_dir).await;
}
