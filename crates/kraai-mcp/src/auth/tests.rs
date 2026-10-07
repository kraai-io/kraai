#![expect(
    clippy::unwrap_used,
    reason = "OAuth storage fixtures assert credential isolation"
)]

use rmcp::transport::auth::{CredentialStore, StoredCredentials};

use super::{flow::validate_url, store::FileStore};

#[tokio::test]
async fn logout_invalidates_old_stores_and_atomic_writes_are_private() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth/credentials.json");
    let old = FileStore::current(
        path.clone(),
        std::sync::Arc::new(kraai_io::fs::DirectoryBootstrap::new(
            directory.path().to_path_buf(),
            directory.path().join("auth"),
        )),
    )
    .unwrap()
    .reset()
    .await
    .unwrap();
    {
        let _guard = old.lock().await.unwrap();
        old.save(StoredCredentials::new("client".into(), None, vec![], None))
            .await
            .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let current = old.reset().await.unwrap();
    let _guard = old.lock().await.unwrap();
    assert!(old.load().await.is_err());
    assert!(
        old.save(StoredCredentials::new("client".into(), None, vec![], None))
            .await
            .is_err()
    );
    assert!(old.clear().await.is_err());
    assert!(current.load().await.unwrap().is_none());
}

#[test]
fn oauth_endpoint_schemes_and_embedded_credentials_are_restricted() {
    for url in [
        "http://example.com/token",
        "file:///tmp/token",
        "https://user:secret@example.com/token",
        "https://example.com/token#fragment",
    ] {
        assert!(validate_url(url, true).is_err());
    }
    assert!(validate_url("https://auth.example.com/token", false).is_ok());
    assert!(validate_url("http://127.0.0.1:8000/token", true).is_ok());
    assert!(validate_url("http://127.0.0.1:8000/token", false).is_err());
    assert!(validate_url("http://[::1]:8000/token", true).is_ok());
}

#[tokio::test]
async fn old_connection_cannot_overwrite_new_login_status() {
    let directory = tempfile::tempdir().unwrap();
    let auth = super::Auth::new(
        "fixture".into(),
        "https://example.com/mcp".into(),
        Default::default(),
        Some(std::sync::Arc::new(kraai_io::fs::DirectoryBootstrap::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        ))),
        std::sync::Weak::new(),
        tokio::sync::broadcast::channel(8).0,
    );
    let old = auth.store().unwrap().reset().await.unwrap();
    old.reset().await.unwrap();
    auth.publish(super::McpAuthState::Authenticated, None).await;
    auth.required(&old, String::from("Old request failed"))
        .await;
    assert!(matches!(
        auth.status().await.state,
        super::McpAuthState::Authenticated
    ));
}

#[tokio::test]
async fn session_auth_events_are_isolated_from_base_and_sibling_aliases() {
    let config = |name: &str| -> crate::McpConfig {
        toml::from_str(&format!(
            "[servers.{name}]\ntransport = {{ type = 'http', url = 'http://127.0.0.1:9/mcp' }}"
        ))
        .unwrap()
    };
    let base = crate::McpManager::new(config("configured")).unwrap();
    let first = base.with_session_servers(config("attached")).unwrap();
    let second = base.with_session_servers(config("attached")).unwrap();
    let mut base_events = base.subscribe_auth();
    let mut first_events = first.subscribe_auth();
    let mut second_events = second.subscribe_auth();
    let attached = first
        .servers
        .get("attached")
        .unwrap()
        .auth
        .as_ref()
        .unwrap();
    assert!(attached.root.is_none());
    attached.publish(super::McpAuthState::Starting, None).await;
    assert_eq!(first_events.try_recv().unwrap().server, "attached");
    assert!(matches!(
        base_events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        second_events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    base.servers
        .get("configured")
        .unwrap()
        .auth
        .as_ref()
        .unwrap()
        .publish(super::McpAuthState::Starting, None)
        .await;
    assert_eq!(base_events.try_recv().unwrap().server, "configured");
    assert!(matches!(
        first_events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    first.shutdown().await;
    second.shutdown().await;
    base.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn credential_directory_sync_failure_survives_new_stores_and_managers() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let anchor = directory.path().join("anchor");
    fs::create_dir(&anchor).unwrap();
    let root = anchor.join("credentials/nested");
    let config: crate::McpConfig = toml::from_str(
        "[servers.first]\ntransport = { type = 'http', url = 'http://127.0.0.1:9/first' }\n\
         [servers.second]\ntransport = { type = 'http', url = 'http://127.0.0.1:9/second' }",
    )
    .unwrap();
    let manager =
        crate::McpManager::with_auth_storage(config.clone(), anchor.clone(), root.clone()).unwrap();
    let first = manager.auth("first").unwrap();
    let second = manager.auth("second").unwrap();

    fs::set_permissions(&anchor, fs::Permissions::from_mode(0o300)).unwrap();
    if fs::read_dir(&anchor).is_ok() {
        fs::set_permissions(&anchor, fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }
    let attempts = async {
        let first_failed = first.store()?.lock().await.is_err();
        let created = root.is_dir();
        let sibling_failed = second.store()?.lock().await.is_err();
        let retry_failed = first.store()?.lock().await.is_err();
        let independent =
            crate::McpManager::with_auth_storage(config, anchor.clone(), root.clone())?;
        let independent_failed = independent
            .auth("first")
            .map_err(|error| error.to_string())?
            .store()?
            .lock()
            .await
            .is_err();
        Ok::<_, String>((
            first_failed,
            created,
            sibling_failed,
            retry_failed,
            independent,
            independent_failed,
        ))
    }
    .await;
    fs::set_permissions(&anchor, fs::Permissions::from_mode(0o700)).unwrap();

    let (first_failed, created, sibling_failed, retry_failed, independent, independent_failed) =
        attempts.unwrap();
    assert!(first_failed);
    assert!(created);
    assert!(sibling_failed);
    assert!(retry_failed);
    assert!(independent_failed);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);

    let current = first.store().unwrap().reset().await.unwrap();
    {
        let _guard = current.lock().await.unwrap();
        current
            .save(StoredCredentials::new("client".into(), None, vec![], None))
            .await
            .unwrap();
    }
    assert!(first.store().unwrap().load().await.unwrap().is_some());
    let independent_store = independent.auth("first").unwrap().store().unwrap();
    {
        let _guard = independent_store.lock().await.unwrap();
        assert!(independent_store.load().await.unwrap().is_some());
    }
    second.store().unwrap().reset().await.unwrap();
    independent.shutdown().await;
    manager.shutdown().await;
}
