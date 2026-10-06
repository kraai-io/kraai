use super::*;

#[tokio::test]
async fn advertised_legacy_version_initializes_with_headers_and_never_replays_tool_errors() {
    let headers = [
        ("Authorization".into(), "Bearer temporary-secret".into()),
        ("X-Session".into(), "legacy".into()),
    ]
    .into();
    let (manager, state, task) = fixture_with_protocol(headers, true, None).await;
    let prompt = manager.prompt().await;
    assert!(prompt.warnings.is_empty(), "{:?}", prompt.warnings);
    assert_eq!(state.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(state.initializations.load(Ordering::SeqCst), 1);
    assert_eq!(
        *state.initialize_versions.lock().unwrap(),
        vec![json!("2025-06-18")]
    );
    assert!(
        manager
            .execute(McpRequest::Call {
                server: "fixture".into(),
                tool: "echo".into(),
                arguments: Default::default()
            })
            .await
            .is_ok()
    );
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    let failed = manager
        .execute(McpRequest::Call {
            server: "fixture".into(),
            tool: "bad-request".into(),
            arguments: Default::default(),
        })
        .await;
    assert!(failed.unwrap_err().contains("not retried"));
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    assert_eq!(state.discoveries.load(Ordering::SeqCst), 1);
    assert_eq!(state.initializations.load(Ordering::SeqCst), 1);
    manager.shutdown().await;
    task.abort();
}

#[tokio::test]
async fn authentication_and_untyped_http_errors_do_not_trigger_pinned_version_retry() {
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::BAD_REQUEST,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let (manager, state, task) =
            fixture_with_protocol(BTreeMap::new(), false, Some(status)).await;
        let prompt = manager.prompt().await;
        assert!(!prompt.warnings.is_empty(), "{status}");
        assert_eq!(state.discoveries.load(Ordering::SeqCst), 1, "{status}");
        let expected = if status == StatusCode::BAD_REQUEST {
            vec![json!(rmcp::model::ProtocolVersion::LATEST)]
        } else {
            Vec::new()
        };
        assert_eq!(
            state.initializations.load(Ordering::SeqCst),
            expected.len(),
            "{status}"
        );
        assert_eq!(
            *state.initialize_versions.lock().unwrap(),
            expected,
            "{status}"
        );
        manager.shutdown().await;
        task.abort();
    }
}
