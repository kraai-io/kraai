use std::path::Path;

use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

use crate::support::Harness;

#[tokio::test]
async fn absolute_parent_components_resolve_to_the_stored_workspace() -> Result<()> {
    let mut harness = Harness::new(vec![]).await?;
    tokio::fs::create_dir(harness.root.path().join("child")).await?;
    let alias = harness.root.path().join("child/..");
    assert_workspace_alias_round_trip(&mut harness, &alias).await?;
    harness.stop().await
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_workspace_can_be_loaded_by_its_canonical_path_or_alias() -> Result<()> {
    let mut harness = Harness::new(vec![]).await?;
    let alias = harness.root.path().join("alias");
    tokio::fs::symlink(harness.root.path(), &alias).await?;
    assert_workspace_alias_round_trip(&mut harness, &alias).await?;
    harness.stop().await
}

async fn assert_workspace_alias_round_trip(harness: &mut Harness, alias: &Path) -> Result<()> {
    harness.initialize().await?;
    let created = harness
        .request(1, "session/new", json!({"cwd":alias,"mcpServers":[]}))
        .await?;
    let session = created
        .last()
        .and_then(|value| value.pointer("/result/sessionId"))
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("workspace alias rejected: {created:?}"))?
        .to_owned();
    harness.restart().await?;
    harness.initialize().await?;
    let canonical = tokio::fs::canonicalize(harness.root.path()).await?;
    for (id, cwd) in [(2, canonical.as_path()), (3, alias)] {
        let loaded = harness
            .request(
                id,
                "session/load",
                json!({"sessionId":session,"cwd":cwd,"mcpServers":[]}),
            )
            .await?;
        assert!(
            loaded
                .last()
                .is_some_and(|value| value.get("result").is_some()),
            "failed to reload canonical workspace: {loaded:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn rejects_missing_file_and_mismatched_workspace_paths() -> Result<()> {
    let mut harness = Harness::new(vec![]).await?;
    let missing = harness.root.path().join("missing");
    let file = harness.root.path().join("regular-file");
    let other = harness.root.path().join("other-workspace");
    tokio::fs::write(&file, "not a directory").await?;
    tokio::fs::create_dir(&other).await?;
    harness.initialize().await?;
    for (id, cwd) in [(2, &missing), (3, &file)] {
        let rejected = harness
            .request(id, "session/new", json!({"cwd":cwd,"mcpServers":[]}))
            .await?;
        assert!(
            rejected
                .last()
                .is_some_and(|value| value.get("error").is_some()),
            "accepted an invalid workspace: {rejected:?}"
        );
    }
    let session = harness.session().await?;
    harness.restart().await?;
    harness.initialize().await?;
    for (id, cwd) in [(4, &missing), (5, &file), (6, &other)] {
        let rejected = harness
            .request(
                id,
                "session/load",
                json!({"sessionId":session,"cwd":cwd,"mcpServers":[]}),
            )
            .await?;
        assert!(
            rejected
                .last()
                .is_some_and(|value| value.get("error").is_some()),
            "loaded a session with the wrong workspace: {rejected:?}"
        );
    }
    let loaded = harness
        .request(
            7,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    assert!(
        loaded
            .last()
            .is_some_and(|value| value.get("result").is_some())
    );
    harness.stop().await
}
