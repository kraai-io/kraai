use super::*;

async fn revisions(persistence: &Persistence) -> Result<Vec<i64>> {
    let mut revisions = Vec::new();
    for id in ["fork-a", "fork-b"] {
        revisions.push(
            persistence
                .sessions()
                .observe(id)
                .await?
                .ok_or_else(|| color_eyre::eyre::eyre!("Missing fork {id}"))?
                .revision,
        );
    }
    Ok(revisions)
}

async fn ensure_revisions_advanced(persistence: &Persistence, before: Vec<i64>) -> Result<()> {
    ensure!(
        revisions(persistence).await?
            == before
                .iter()
                .map(|revision| revision + 1)
                .collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn shared_records_fence_and_notify_every_surviving_fork() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let a = Persistence::open(directory.path()).await?;
    a.sessions().save(&session("original")).await?;
    let root = a
        .conversations()
        .append_message(request("original", "original"))
        .await?;
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
    a.compactions().save(&checkpoint).await?;
    for id in ["fork-a", "fork-b"] {
        let mut fork = session(id);
        fork.tip_id = Some(root.message.id.clone());
        a.sessions().save(&fork).await?;
        for _ in 0..3 {
            a.conversations()
                .append_message(request(id, "fork child"))
                .await?;
        }
    }
    a.sessions().delete("original").await?;
    let b = Persistence::open(directory.path()).await?;
    let observer = Persistence::open(directory.path()).await?;
    a.sessions().claim_turn("fork-b").await?;
    b.sessions().claim_turn("fork-a").await?;
    let before = revisions(&observer).await?;
    let mut changed = root.message.clone();
    changed.content = ConversationItem::User {
        content: "changed".into(),
    };
    ensure!(observer.messages().save(&changed).await.is_err());
    ensure!(observer.messages().delete(&changed.id).await.is_err());
    ensure!(observer.compactions().save(&checkpoint).await.is_err());
    ensure!(observer.compactions().delete(&changed.id).await.is_err());
    ensure!(a.messages().save(&changed).await.is_err());
    ensure!(b.messages().save(&changed).await.is_err());
    ensure!(b.compactions().save(&checkpoint).await.is_err());
    ensure!(revisions(&observer).await? == before);
    ensure!(
        observer
            .messages()
            .get(&changed.id)
            .await?
            .is_some_and(|message| message.content.text() == Some("original"))
    );
    ensure!(observer.compactions().get(&changed.id).await?.as_ref() == Some(&checkpoint));
    for id in ["fork-a", "fork-b"] {
        ensure!(observer.messages().read_conversation(id).await?.is_some());
    }
    a.sessions().release_turn("fork-b").await?;
    let before = revisions(&observer).await?;
    b.messages().save(&changed).await?;
    ensure_revisions_advanced(&observer, before).await?;
    let before = revisions(&observer).await?;
    b.compactions().save(&checkpoint).await?;
    ensure_revisions_advanced(&observer, before).await?;
    let before = revisions(&observer).await?;
    b.compactions().delete(&changed.id).await?;
    ensure_revisions_advanced(&observer, before).await?;
    b.sessions().release_turn("fork-a").await?;
    let before = revisions(&observer).await?;
    observer.messages().save(&changed).await?;
    ensure_revisions_advanced(&observer, before).await?;
    let before = revisions(&observer).await?;
    observer.compactions().save(&checkpoint).await?;
    ensure_revisions_advanced(&observer, before).await?;
    let before = revisions(&observer).await?;
    observer.compactions().delete(&changed.id).await?;
    ensure_revisions_advanced(&observer, before).await?;
    Ok(())
}

#[tokio::test]
async fn new_unlinked_children_are_fenced_by_surviving_parent_sessions() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let owner = Persistence::open(directory.path()).await?;
    owner.sessions().save(&session("original")).await?;
    let mut root = owner
        .conversations()
        .append_message(request("original", "parent"))
        .await?
        .message;
    let mut fork = session("fork");
    fork.tip_id = Some(root.id.clone());
    owner.sessions().save(&fork).await?;
    owner.sessions().delete("original").await?;
    owner.sessions().claim_turn("fork").await?;
    let observer = Persistence::open(directory.path()).await?;
    root.parent_id = Some(root.id.clone());
    root.id = MessageId::new("unlinked-child");
    ensure!(observer.messages().save(&root).await.is_err());
    ensure!(observer.messages().get(&root.id).await?.is_none());
    owner.messages().save(&root).await?;
    ensure!(observer.messages().get(&root.id).await?.is_some());
    ensure!(observer.messages().save(&root).await.is_err());
    root.parent_id = None;
    ensure!(observer.messages().save(&root).await.is_err());
    Ok(())
}

#[tokio::test]
async fn deleted_original_lease_cannot_mutate_retained_history() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let stale_owner = Persistence::open(directory.path()).await?;
    stale_owner.sessions().save(&session("original")).await?;
    stale_owner.sessions().claim_turn("original").await?;
    let mut root = stale_owner
        .conversations()
        .append_message(request("original", "retained"))
        .await?
        .message;
    let mut fork = session("fork");
    fork.tip_id = Some(root.id.clone());
    stale_owner.sessions().save(&fork).await?;
    stale_owner.sessions().delete("original").await?;
    root.content = ConversationItem::User {
        content: "stale owner change".into(),
    };
    ensure!(stale_owner.messages().save(&root).await.is_err());
    ensure!(stale_owner.messages().delete(&root.id).await.is_err());
    let observer = Persistence::open(directory.path()).await?;
    ensure!(
        observer
            .messages()
            .get(&root.id)
            .await?
            .is_some_and(|message| message.content.text() == Some("retained"))
    );
    observer.messages().save(&root).await?;
    ensure!(stale_owner.messages().save(&root).await.is_err());
    stale_owner.sessions().release_turn("original").await?;
    stale_owner.messages().save(&root).await?;
    Ok(())
}
