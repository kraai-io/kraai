use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, ensure};

struct Service {
    child: Child,
    root: PathBuf,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn proxy_service_exposes_only_ephemeral_credentials_and_flushes_on_stdin_close() -> Result<()> {
    let root = std::env::temp_dir().join(format!("kraai-proxy-service-{}", ulid::Ulid::generate()));
    fs::create_dir(&root)?;
    let state = root.join("state");
    let child = Command::new(env!("CARGO_BIN_EXE_kraai-eval"))
        .args([
            "proxy",
            "--proxy",
            "openai",
            "--credential-env",
            "KRAAI_PROXY_TEST_SECRET",
            "--state-dir",
        ])
        .arg(&state)
        .env("KRAAI_PROXY_TEST_SECRET", "controller-secret-do-not-expose")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut service = Service { child, root };
    let started = Instant::now();
    while !state.join("ready.json").exists() {
        ensure!(
            service.child.try_wait()?.is_none(),
            "proxy exited before readiness"
        );
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "proxy never became ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let bytes = fs::read(state.join("ready.json"))?;
    let ready: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(!String::from_utf8_lossy(&bytes).contains("controller-secret-do-not-expose"));
    ensure!(
        ready
            .pointer("/environment/OPENAI_API_KEY")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|token| token.len() >= 32)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(fs::metadata(state.join("ready.json"))?.permissions().mode() & 0o777 == 0o600);
        ensure!(fs::metadata(&state)?.permissions().mode() & 0o777 == 0o700);
    }
    let base = reqwest::Url::parse(
        ready
            .get("base_url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    )?;
    let mut connection =
        TcpStream::connect((Ipv4Addr::LOCALHOST, base.port().unwrap_or_default()))?;
    connection.set_read_timeout(Some(Duration::from_secs(5)))?;
    connection
        .write_all(b"GET /blocked HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    connection.read_to_string(&mut response)?;
    ensure!(response.starts_with("HTTP/1.1 404"));
    drop(service.child.stdin.take());
    let stopped = Instant::now();
    loop {
        if let Some(status) = service.child.try_wait()? {
            ensure!(status.success(), "proxy failed to shut down");
            break;
        }
        ensure!(
            stopped.elapsed() < Duration::from_secs(10),
            "proxy did not flush on EOF"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let metrics: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("metrics.json"))?)?;
    ensure!(metrics.get("requests").and_then(serde_json::Value::as_u64) == Some(1));
    ensure!(!state.join("ready.json").exists());
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("identity.json"))?)?;
    ensure!(identity.get("credential_fingerprint").is_none());
    ensure!(
        identity
            .get("transport_revision")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
    );
    Ok(())
}
