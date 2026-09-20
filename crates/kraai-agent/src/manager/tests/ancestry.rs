use super::super::*;
use super::common::{cleanup_dir, test_manager};

#[tokio::test]
async fn source_ancestry_queries_stop_after_finding_all_candidates() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let older = manager
        .add_message(&session_id, ChatRole::User, "older".into(), None)
        .await?;
    let first = manager
        .add_message(&session_id, ChatRole::Assistant, "first".into(), None)
        .await?;
    let second = manager
        .add_message(&session_id, ChatRole::Assistant, "second".into(), None)
        .await?;
    let tip = manager
        .add_message(&session_id, ChatRole::User, "tip".into(), None)
        .await?;
    for id in [&older, &first, &second, &tip] {
        manager.message_store.unload(id).await;
    }
    tokio::fs::write(
        data_dir.join("messages").join(format!("{older}.json")),
        b"invalid older message",
    )
    .await?;

    let candidates = HashSet::from([first, second]);
    assert_eq!(
        manager
            .reachable_message_ids(&session_id, candidates.clone())
            .await?,
        candidates
    );
    assert!(manager.message_store.list_hot().await?.is_empty());
    let error = manager
        .reachable_message_ids(&session_id, HashSet::from([MessageId::new("missing")]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Failed to parse message file"));
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn source_ancestry_queries_distinguish_roots_missing_messages_and_cycles() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session_id = manager.create_session().await?;
    let missing = MessageId::new("missing");
    let root = manager
        .add_message(&session_id, ChatRole::User, "root".into(), None)
        .await?;
    assert!(
        manager
            .reachable_message_ids(&session_id, HashSet::from([missing.clone()]))
            .await?
            .is_empty()
    );
    assert_eq!(
        manager
            .reachable_message_ids(&session_id, HashSet::from([root.clone()]))
            .await?,
        HashSet::from([root.clone()])
    );

    manager.set_tip(&session_id, Some(missing.clone())).await?;
    let error = manager
        .reachable_message_ids(&session_id, HashSet::from([missing.clone()]))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Message missing disappeared while reading session history"
    );

    let mut message = manager.message_store.get(&root).await?.unwrap();
    message.parent_id = Some(root.clone());
    manager.message_store.save(&message).await?;
    manager.set_tip(&session_id, Some(root.clone())).await?;
    let error = manager
        .reachable_message_ids(&session_id, HashSet::from([missing]))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("Corrupt message parent graph: cycle repeats message {root}")
    );
    cleanup_dir(data_dir).await;
    Ok(())
}
