use super::*;

async fn revisions(persistence: &Persistence) -> Vec<i64> {
    let mut revisions = Vec::new();
    for id in ["fork-a", "fork-b"] {
        revisions.push(
            persistence
                .sessions()
                .observe(&SessionId::new(id))
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("Missing fork {id}"))
                .revision,
        );
    }
    revisions
}

async fn ensure_revisions_advanced(persistence: &Persistence, before: Vec<i64>) {
    assert!(
        revisions(persistence).await
            == before
                .iter()
                .map(|revision| revision + 1)
                .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn shared_records_fence_and_notify_every_surviving_fork() {
    let directory = tempfile::tempdir().unwrap();
    let a = open_persistence(directory.path()).await.unwrap();
    a.sessions().save(&session("original")).await.unwrap();
    let root = a
        .conversations()
        .append_message(request("original", "original"))
        .await
        .unwrap();
    let checkpoint = CompactionCheckpoint {
        covered_through: root.message.id.clone(),
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
    a.compactions().save(&checkpoint).await.unwrap();
    for id in ["fork-a", "fork-b"] {
        let mut fork = session(id);
        fork.tip_id = Some(root.message.id.clone());
        a.sessions().save(&fork).await.unwrap();
        for _ in 0..3 {
            a.conversations()
                .append_message(request(id, "fork child"))
                .await
                .unwrap();
        }
    }
    a.sessions()
        .delete(&SessionId::new("original"))
        .await
        .unwrap();
    let b = open_persistence(directory.path()).await.unwrap();
    let observer = open_persistence(directory.path()).await.unwrap();
    a.sessions()
        .claim_turn(&SessionId::new("fork-b"))
        .await
        .unwrap();
    b.sessions()
        .claim_turn(&SessionId::new("fork-a"))
        .await
        .unwrap();
    let before = revisions(&observer).await;
    let mut changed = root.message.clone();
    changed.content = ConversationItem::User {
        content: "changed".into(),
    };
    assert!(observer.messages().save(&changed).await.is_err());
    assert!(observer.messages().delete(&changed.id).await.is_err());
    assert!(observer.compactions().save(&checkpoint).await.is_err());
    assert!(observer.compactions().delete(&changed.id).await.is_err());
    assert!(a.messages().save(&changed).await.is_err());
    assert!(b.messages().save(&changed).await.is_err());
    assert!(b.compactions().save(&checkpoint).await.is_err());
    assert!(revisions(&observer).await == before);
    assert!(
        observer
            .messages()
            .get(&changed.id)
            .await
            .unwrap()
            .is_some_and(|message| message.content.text() == Some("original"))
    );
    assert!(
        observer
            .compactions()
            .get(&changed.id)
            .await
            .unwrap()
            .as_ref()
            == Some(&checkpoint)
    );
    for id in ["fork-a", "fork-b"] {
        assert!(
            observer
                .messages()
                .read_conversation(&SessionId::new(id))
                .await
                .unwrap()
                .is_some()
        );
    }
    a.sessions()
        .release_turn(&SessionId::new("fork-b"))
        .await
        .unwrap();
    let before = revisions(&observer).await;
    b.messages().save(&changed).await.unwrap();
    ensure_revisions_advanced(&observer, before).await;
    let before = revisions(&observer).await;
    b.compactions().save(&checkpoint).await.unwrap();
    ensure_revisions_advanced(&observer, before).await;
    let before = revisions(&observer).await;
    b.compactions().delete(&changed.id).await.unwrap();
    ensure_revisions_advanced(&observer, before).await;
    b.sessions()
        .release_turn(&SessionId::new("fork-a"))
        .await
        .unwrap();
    let before = revisions(&observer).await;
    observer.messages().save(&changed).await.unwrap();
    ensure_revisions_advanced(&observer, before).await;
    let before = revisions(&observer).await;
    observer.compactions().save(&checkpoint).await.unwrap();
    ensure_revisions_advanced(&observer, before).await;
    let before = revisions(&observer).await;
    observer.compactions().delete(&changed.id).await.unwrap();
    ensure_revisions_advanced(&observer, before).await;
}

#[tokio::test]
async fn new_unlinked_children_are_fenced_by_surviving_parent_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let owner = open_persistence(directory.path()).await.unwrap();
    owner.sessions().save(&session("original")).await.unwrap();
    let mut root = owner
        .conversations()
        .append_message(request("original", "parent"))
        .await
        .unwrap()
        .message;
    let mut fork = session("fork");
    fork.tip_id = Some(root.id.clone());
    owner.sessions().save(&fork).await.unwrap();
    owner
        .sessions()
        .delete(&SessionId::new("original"))
        .await
        .unwrap();
    owner
        .sessions()
        .claim_turn(&SessionId::new("fork"))
        .await
        .unwrap();
    let observer = open_persistence(directory.path()).await.unwrap();
    root.parent_id = Some(root.id.clone());
    root.id = MessageId::new("unlinked-child");
    assert!(observer.messages().save(&root).await.is_err());
    assert!(observer.messages().get(&root.id).await.unwrap().is_none());
    owner.messages().save(&root).await.unwrap();
    assert!(observer.messages().get(&root.id).await.unwrap().is_some());
    assert!(observer.messages().save(&root).await.is_err());
    root.parent_id = None;
    assert!(observer.messages().save(&root).await.is_err());
}

#[tokio::test]
async fn deleted_original_lease_cannot_mutate_retained_history() {
    let directory = tempfile::tempdir().unwrap();
    let stale_owner = open_persistence(directory.path()).await.unwrap();
    stale_owner
        .sessions()
        .save(&session("original"))
        .await
        .unwrap();
    stale_owner
        .sessions()
        .claim_turn(&SessionId::new("original"))
        .await
        .unwrap();
    let mut root = stale_owner
        .conversations()
        .append_message(request("original", "retained"))
        .await
        .unwrap()
        .message;
    let mut fork = session("fork");
    fork.tip_id = Some(root.id.clone());
    stale_owner.sessions().save(&fork).await.unwrap();
    stale_owner
        .sessions()
        .delete(&SessionId::new("original"))
        .await
        .unwrap();
    root.content = ConversationItem::User {
        content: "stale owner change".into(),
    };
    assert!(stale_owner.messages().save(&root).await.is_err());
    assert!(stale_owner.messages().delete(&root.id).await.is_err());
    let observer = open_persistence(directory.path()).await.unwrap();
    assert!(
        observer
            .messages()
            .get(&root.id)
            .await
            .unwrap()
            .is_some_and(|message| message.content.text() == Some("retained"))
    );
    observer.messages().save(&root).await.unwrap();
    assert!(stale_owner.messages().save(&root).await.is_err());
    stale_owner
        .sessions()
        .release_turn(&SessionId::new("original"))
        .await
        .unwrap();
    stale_owner.messages().save(&root).await.unwrap();
}
