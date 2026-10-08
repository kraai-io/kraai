use super::*;

#[tokio::test]
async fn message_ids_do_not_load_message_payloads() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3"))?;
    connection.execute(
        "INSERT INTO records(kind, id, data) VALUES ('message', 'message', 'invalid JSON'), ('usage', 'usage', 'invalid JSON')",
        [],
    )?;
    ensure!(
        persistence.messages().list_ids().await?
            == std::collections::HashSet::from([MessageId::new("message")])
    );
    ensure!(
        persistence
            .messages()
            .get(&MessageId::new("message"))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn database_creation_records_the_schema_version() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("session")).await?;
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3"))?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(version == 1);
    let reopened = Persistence::open(directory.path()).await?;
    ensure!(reopened.sessions().get("session").await?.is_some());
    Ok(())
}

#[tokio::test]
async fn unsupported_schema_version_is_rejected_without_changing_the_database() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3"))?;
    connection.pragma_update(None, "user_version", 2)?;
    ensure!(Persistence::open(directory.path()).await.is_err());
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(version == 2);
    let tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table'",
        [],
        |row| row.get(0),
    )?;
    ensure!(tables == 0);
    Ok(())
}

#[tokio::test]
async fn deletion_preserves_branched_history_and_removes_unlinked_messages() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let persistence = Persistence::open(directory.path()).await?;
    persistence.sessions().save(&session("original")).await?;
    let mut original_ids = Vec::new();
    for _ in 0..16 {
        let appended = persistence
            .conversations()
            .append_message(request("original", "shared"))
            .await?;
        original_ids.push(appended.message.id);
    }
    let mut fork = session("fork");
    fork.tip_id = original_ids.last().cloned();
    persistence.sessions().save(&fork).await?;
    let mut other_fork = session("other-fork");
    other_fork.tip_id = original_ids.first().cloned();
    persistence.sessions().save(&other_fork).await?;
    for _ in 0..16 {
        persistence
            .conversations()
            .append_message(request("original", "exclusive"))
            .await?;
    }
    let mut unlinked = persistence
        .conversations()
        .append_message(request("fork", "fork message"))
        .await?
        .message;
    let fork_message_id = unlinked.id.clone();
    unlinked.id = MessageId::new("unlinked-original-message");
    unlinked.parent_id = original_ids.last().cloned();
    persistence.messages().save(&unlinked).await?;
    persistence.sessions().delete("original").await?;
    let retained = persistence.messages().list_ids().await?;
    ensure!(retained.len() == original_ids.len() + 1);
    ensure!(retained.contains(&fork_message_id));
    for id in &original_ids {
        ensure!(retained.contains(id));
    }
    ensure!(!retained.contains(&unlinked.id));
    persistence.sessions().delete("fork").await?;
    ensure!(
        persistence.messages().list_ids().await?
            == original_ids.first().cloned().into_iter().collect()
    );
    persistence.sessions().delete("other-fork").await?;
    ensure!(persistence.messages().list_ids().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn deletion_cannot_remove_another_clients_owned_unlinked_message() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let owner = Persistence::open(directory.path()).await?;
    owner.sessions().save(&session("owner")).await?;
    let root = owner
        .conversations()
        .append_message(request("owner", "root"))
        .await?;
    let mut fork = session("fork");
    fork.tip_id = Some(root.message.id.clone());
    owner.sessions().save(&fork).await?;
    let mut metadata = owner
        .sessions()
        .get("owner")
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing session"))?;
    metadata.tip_id = None;
    owner.sessions().save(&metadata).await?;
    owner.sessions().claim_turn("owner").await?;
    let observer = Persistence::open(directory.path()).await?;
    ensure!(observer.sessions().delete("fork").await.is_err());
    ensure!(observer.sessions().get("fork").await?.is_some());
    ensure!(observer.messages().get(&root.message.id).await?.is_some());
    Ok(())
}
