use super::*;

fn attachment(alias: &str, fixture: &Fixture) -> McpConfig {
    McpConfig {
        servers: [(
            alias.into(),
            ServerConfig {
                enabled: true,
                description: String::new(),
                startup_timeout_secs: 2,
                call_timeout_secs: 1,
                transport: TransportConfig::Http {
                    url: fixture.url.clone(),
                    headers: fixture.headers.clone(),
                    bearer_token_env: None,
                    oauth: None,
                },
            },
        )]
        .into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn headers_are_forwarded_without_oauth_and_redirects_remain_disabled() {
    let headers = [
        ("aUtHoRiZaTiOn".into(), "Bearer session-secret".into()),
        ("X-Session".into(), "first-session".into()),
    ]
    .into();
    let (manager, state, task) = fixture_with_headers(headers).await;
    assert!(manager.prompt().await.warnings.is_empty());
    manager
        .execute(McpRequest::Call {
            server: "fixture".into(),
            tool: "echo".into(),
            arguments: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    assert!(
        manager
            .auth_statuses()
            .await
            .iter()
            .all(|status| matches!(status.state, kraai_mcp::McpAuthState::Unavailable))
    );
    assert!(manager.start_login("fixture").await.is_err());
    let mut redirected = attachment("redirected", &state);
    if let TransportConfig::Http { url, .. } =
        &mut redirected.servers.get_mut("redirected").unwrap().transport
    {
        *url = state.url.replace("/mcp", "/redirect");
    }
    let redirect = McpManager::new(redirected).unwrap();
    assert!(!redirect.prompt().await.warnings.is_empty());
    assert_eq!(state.discoveries.load(Ordering::SeqCst), 1);
    redirect.shutdown().await;
    manager.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn overlay_shutdown_preserves_shared_and_sibling_connections() {
    let (base, base_state, base_task) = fixture().await;
    let (unused_first, first_state, first_task) = fixture().await;
    let (unused_second, second_state, second_task) = fixture().await;
    let first = base
        .with_session_servers(attachment("attached", &first_state))
        .unwrap();
    let second = base
        .with_session_servers(attachment("attached", &second_state))
        .unwrap();
    assert!(first.prompt().await.warnings.is_empty());
    assert!(second.prompt().await.warnings.is_empty());
    assert_eq!(base_state.discoveries.load(Ordering::SeqCst), 1);
    assert!(
        base.execute(McpRequest::Tools {
            server: "attached".into()
        })
        .await
        .is_err()
    );
    first.shutdown().await;
    assert!(base.prompt().await.warnings.is_empty());
    assert!(second.prompt().await.warnings.is_empty());
    assert_eq!(base_state.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(second_state.discoveries.load(Ordering::SeqCst), 1);
    assert!(first.prompt().await.warnings.is_empty());
    assert_eq!(first_state.discoveries.load(Ordering::SeqCst), 2);
    base.shutdown().await;
    assert!(second.prompt().await.warnings.is_empty());
    assert_eq!(base_state.discoveries.load(Ordering::SeqCst), 2);
    first.shutdown().await;
    second.shutdown().await;
    base.shutdown().await;
    unused_first.shutdown().await;
    unused_second.shutdown().await;
    for task in [base_task, first_task, second_task] {
        task.abort();
    }
}

#[tokio::test]
async fn attachment_credentials_do_not_use_the_base_auth_store() {
    let headers = [("Authorization".into(), "Bearer ephemeral-secret".into())].into();
    let (unused, state, task) = fixture_with_headers(headers).await;
    let root = tempfile::tempdir().unwrap();
    let base =
        McpManager::with_auth_storage(McpConfig::default(), root.path().to_path_buf()).unwrap();
    let overlay = base
        .with_session_servers(attachment("temporary", &state))
        .unwrap();
    assert!(overlay.prompt().await.warnings.is_empty());
    assert!(overlay.start_login("temporary").await.is_err());
    assert!(
        tokio::fs::read_dir(root.path())
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none()
    );
    overlay.shutdown().await;
    base.shutdown().await;
    unused.shutdown().await;
    task.abort();
}
