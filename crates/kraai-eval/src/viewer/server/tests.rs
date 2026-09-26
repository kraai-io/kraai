use super::*;
use color_eyre::eyre::{Result, ensure};

struct Server {
    root: PathBuf,
    url: String,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Server {
    async fn start() -> Result<Self> {
        let root = std::env::temp_dir().join(format!("kraai-viewer-{}", ulid::Ulid::generate()));
        let assets = root.join("assets");
        std::fs::create_dir_all(&assets)?;
        std::fs::write(
            assets.join("index.html"),
            "<title>Benchmark results</title>",
        )?;
        std::fs::write(assets.join("app.js"), "export {};")?;
        std::fs::write(root.join("secret.html"), "private")?;
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let router = router(root.join("cache"), assets.canonicalize()?);
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        Ok(Self {
            root,
            url: format!("http://{address}"),
            task,
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn serves_assets_and_an_empty_catalog_without_running_evaluations() -> Result<()> {
    let server = Server::start().await?;
    let client = reqwest::Client::new();
    let response = client.get(&server.url).send().await?;
    ensure!(response.status() == StatusCode::OK);
    ensure!(
        response.headers().get(header::X_CONTENT_TYPE_OPTIONS)
            == Some(&HeaderValue::from_static("nosniff"))
    );
    let body = response.text().await?;
    ensure!(body.contains("Benchmark results"));
    let response = client
        .get(format!("{}/api/catalog", server.url))
        .send()
        .await?;
    ensure!(response.status() == StatusCode::OK);
    let body: serde_json::Value = response.json().await?;
    ensure!(body.get("attempts") == Some(&serde_json::json!([])));
    ensure!(!server.root.join("cache").exists());
    let response = client.get(format!("{}/app.js", server.url)).send().await?;
    ensure!(
        response.headers().get(header::CONTENT_TYPE)
            == Some(&HeaderValue::from_static("text/javascript; charset=utf-8"))
    );
    Ok(())
}

#[tokio::test]
async fn rejects_external_hosts_origins_and_unregistered_artifacts() -> Result<()> {
    let server = Server::start().await?;
    let client = reqwest::Client::new();
    for name in [header::HOST, header::ORIGIN] {
        let response = client
            .get(format!("{}/api/catalog", server.url))
            .header(name, "https://example.com")
            .send()
            .await?;
        ensure!(response.status() == StatusCode::FORBIDDEN);
    }
    for path in [
        "/secret.html",
        "/%2e%2e/secret.html",
        "/api/attempts/unknown/logs/secret.html",
    ] {
        let response = client.get(format!("{}{path}", server.url)).send().await?;
        ensure!(response.status() == StatusCode::NOT_FOUND);
        let body = response.text().await?;
        ensure!(!body.contains("private"));
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_asset_symlinks_outside_the_asset_directory() -> Result<()> {
    let server = Server::start().await?;
    std::os::unix::fs::symlink(
        server.root.join("secret.html"),
        server.root.join("assets/escape.html"),
    )?;
    let response = reqwest::get(format!("{}/escape.html", server.url)).await?;
    ensure!(response.status() == StatusCode::NOT_FOUND);
    Ok(())
}

#[test]
fn normalizes_network_authorities_and_rejects_invalid_hosts() {
    assert_eq!(
        viewer_authority("127.0.0.1"),
        viewer_authority("127.0.0.1:80")
    );
    assert_eq!(
        viewer_authority("LOCALHOST"),
        viewer_authority("localhost:80")
    );
    assert_eq!(
        viewer_authority("[::1]:80"),
        viewer_authority("[0:0:0:0:0:0:0:1]")
    );
    assert_eq!(
        viewer_authority("MAKAI:3928"),
        viewer_authority("makai:3928")
    );
    assert_ne!(
        viewer_authority("makai:3928"),
        viewer_authority("makai.:3928")
    );
    for value in [
        "localhost:",
        "localhost:0",
        "localhost:65536",
        "localhost:invalid",
        "makai other",
        "user@127.0.0.1",
        "[127.0.0.1]",
        "::1",
        "https://127.0.0.1",
        "127.0.0.1/path",
        "127.0.0.1?query",
        "127.0.0.1#fragment",
    ] {
        assert!(
            viewer_authority(value).is_none(),
            "accepted invalid authority {value}"
        );
    }
}

#[tokio::test]
async fn accepts_network_hosts_and_forwarded_ports_but_rejects_cross_origin_requests() -> Result<()>
{
    let server = Server::start().await?;
    let client = reqwest::Client::new();
    for host in [
        "100.64.0.2:3928",
        "localhost:49123",
        "127.0.0.1:49123",
        "[fd00::1]:3928",
        "makai:3928",
        "makai.netbird.cloud:3928",
    ] {
        let response = client
            .get(format!("{}/api/catalog", server.url))
            .header(header::HOST, host)
            .header(header::ORIGIN, format!("http://{host}"))
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::OK,
            "rejected authority {host}"
        );
    }
    for origin in [
        "http://100.64.0.3:3928",
        "http://100.64.0.2:3929",
        "https://100.64.0.2:3928",
        "http://example.com",
        "null",
    ] {
        let response = client
            .get(format!("{}/api/catalog", server.url))
            .header(header::HOST, "100.64.0.2:3928")
            .header(header::ORIGIN, origin)
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::FORBIDDEN,
            "accepted origin {origin}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn failed_refresh_keeps_previous_results_and_recovers_after_a_cooldown() -> Result<()> {
    let server = Server::start().await?;
    let state = AppState {
        root: server.root.join("cache"),
        assets: server.root.join("assets"),
        catalog: Arc::new(Mutex::new(None)),
    };
    let (original, _) = state
        .snapshot()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.1))?;
    std::fs::write(&state.root, "not a directory")?;
    expire(&state).await;
    let (stale, warning) = state
        .snapshot()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.1))?;
    ensure!(Arc::ptr_eq(&original, &stale));
    ensure!(
        warning
            .as_ref()
            .is_some_and(|warning| warning.contains("refresh failed"))
    );
    std::fs::remove_file(&state.root)?;
    std::fs::create_dir(&state.root)?;
    let (_, warning) = state
        .snapshot()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.1))?;
    ensure!(
        warning.is_some(),
        "refresh should respect its retry cooldown"
    );
    expire(&state).await;
    let (current, warning) = state
        .snapshot()
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.1))?;
    ensure!(warning.is_none());
    ensure!(!Arc::ptr_eq(&original, &current));
    Ok(())
}

async fn expire(state: &AppState) {
    if let Some(cached) = state.catalog.lock().await.as_mut() {
        cached.loaded_at = Instant::now() - Duration::from_secs(5);
    }
}
