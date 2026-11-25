use serde_json::json;

use super::*;

fn snapshot(fetched_at: u64, context: usize) -> Value {
    json!({
        "fetched_at": fetched_at,
        "providers": {"openai": {"models": {"model": {"limit": {"context": context}}}}}
    })
}

async fn context(catalog: &ModelCatalog) -> Option<usize> {
    catalog
        .metadata(Some("openai"), None, "model")
        .await
        .and_then(|metadata| metadata.max_context)
}

#[tokio::test]
async fn other_schema_versions_are_discarded_and_rebuilt_instead_of_migrated() {
    for version in [
        crate::model_catalog::store::SCHEMA_VERSION - 1,
        crate::model_catalog::store::SCHEMA_VERSION + 1,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models.sqlite3");
        let legacy = directory.path().join("models.json");
        std::fs::write(&legacy, "{}").unwrap();
        let old = Connection::open(&path).unwrap();
        old.execute_batch(&format!(
            "CREATE TABLE models (provider TEXT, model TEXT, context INTEGER);
             INSERT INTO models VALUES ('openai', 'model', 1);
             CREATE TABLE meta (fetched_at INTEGER);
             INSERT INTO meta VALUES ({});
             CREATE VIEW leftover AS SELECT * FROM models;
             PRAGMA user_version = {version};",
            crate::pricing::now()
        ))
        .unwrap();

        let catalog = ModelCatalog::new(Some(path.clone()));
        catalog.initialize().await;
        assert_eq!(catalog.revision(), 0);
        assert_eq!(context(&catalog).await, None);
        assert_eq!(catalog.observe().await, None);

        install(&catalog, snapshot(123, 4096)).await;
        assert_eq!(context(&catalog).await, Some(4096));
        assert_eq!(catalog.revision(), 1);
        let schema = old
            .prepare("SELECT name FROM sqlite_schema WHERE type IN ('table', 'view') ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(schema, ["cost_tiers", "meta", "models", "providers"]);
        assert_eq!(
            old.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            crate::model_catalog::store::SCHEMA_VERSION
        );
        assert!(!legacy.exists());
    }
}

#[tokio::test]
async fn corrupt_databases_are_replaced_by_the_next_fetch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("models.sqlite3");
    std::fs::write(&path, vec![b'x'; 8192]).unwrap();
    let catalog = ModelCatalog::new(Some(path.clone()));
    catalog.initialize().await;
    assert_eq!(catalog.observe().await, None);
    assert_eq!(context(&catalog).await, None);

    install(&catalog, snapshot(123, 4096)).await;
    assert_eq!(context(&catalog).await, Some(4096));
    let restarted = ModelCatalog::new(Some(path));
    restarted.initialize().await;
    assert_eq!(restarted.revision(), 1);
    assert_eq!(context(&restarted).await, Some(4096));
}

#[tokio::test]
async fn processes_sharing_the_cache_see_whole_catalogs_and_pick_up_refreshes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("models.sqlite3");
    let writer = ModelCatalog::new(Some(path.clone()));
    let reader = ModelCatalog::new(Some(path));
    reader.initialize().await;
    assert_eq!(reader.revision(), 0);

    install(&writer, snapshot(100, 4096)).await;
    assert_eq!(reader.observe().await, Some(100));
    assert_eq!(reader.revision(), 1);
    assert_eq!(context(&reader).await, Some(4096));

    let view = reader.view().await;
    let before = view.metadata_with_discovery(Some("openai"), None, "model", None);
    install(&writer, snapshot(200, 8192)).await;
    let during = view.metadata_with_discovery(Some("openai"), None, "model", None);
    drop(view);
    assert_eq!(
        before.map(|(metadata, _)| metadata.max_context),
        Some(Some(4096))
    );
    assert_eq!(
        during.map(|(metadata, _)| metadata.max_context),
        Some(Some(4096))
    );
    assert_eq!(context(&reader).await, Some(8192));
    assert_eq!(reader.observe().await, Some(200));
    assert_eq!(reader.revision(), 2);
    assert_eq!(reader.observe().await, Some(200));
    assert_eq!(reader.revision(), 2);
}

#[tokio::test]
async fn failed_fetches_keep_the_previous_catalog() {
    let catalog = catalog_from(snapshot(123, 4096)).await;
    for invalid in [
        b"{}".as_slice(),
        b"[]",
        b"{\"openai\":{\"models\":{}}} trailing",
    ] {
        assert!(catalog.install(invalid.to_vec(), 456).await.is_err());
    }
    assert_eq!(context(&catalog).await, Some(4096));
}
