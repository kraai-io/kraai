mod catalog;
mod server;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use color_eyre::eyre::{Context, Result, ensure};

pub async fn serve(cache_dir: PathBuf, port: u16, open_browser: bool) -> Result<()> {
    let assets = std::env::var_os("KRAAI_EVAL_VIEWER_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("packages/eval-viewer/dist"));
    ensure!(
        assets.join("index.html").is_file(),
        "viewer assets not found at {}; build them with `just build-eval-viewer` or use the Nix package",
        assets.display()
    );
    let assets = assets.canonicalize().wrap_err("resolve viewer assets")?;
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
    let address = listener.local_addr()?;
    let url = format!(
        "http://{}",
        SocketAddr::from((Ipv4Addr::LOCALHOST, address.port()))
    );
    let app = server::router(std::path::absolute(cache_dir)?, assets);
    println!("Benchmark results: {url} (listening on {address})");
    println!("Press Ctrl+C to stop the viewer.");
    if open_browser {
        std::thread::spawn(move || {
            if let Err(error) = webbrowser::open(&url) {
                println!("Could not open a browser: {error}. Open the URL above.");
            }
        });
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
