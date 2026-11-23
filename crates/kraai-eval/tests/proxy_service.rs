use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kraai_test_support::HANG_TIMEOUT;

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
fn proxy_service_exposes_only_ephemeral_credentials_and_flushes_on_stdin_close() {
    let root = std::env::temp_dir().join(format!("kraai-proxy-service-{}", ulid::Ulid::generate()));
    fs::create_dir(&root).unwrap();
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
        .spawn()
        .unwrap();
    let mut service = Service { child, root };
    let started = Instant::now();
    while !state.join("ready.json").exists() {
        assert!(
            service.child.try_wait().unwrap().is_none(),
            "proxy exited before readiness"
        );
        assert!(started.elapsed() < HANG_TIMEOUT, "proxy never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    let bytes = fs::read(state.join("ready.json")).unwrap();
    let ready: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("controller-secret-do-not-expose"));
    assert!(
        ready
            .pointer("/environment/OPENAI_API_KEY")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|token| token.len() >= 32)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            fs::metadata(state.join("ready.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
                == 0o600
        );
        assert!(fs::metadata(&state).unwrap().permissions().mode() & 0o777 == 0o700);
    }
    let base = reqwest::Url::parse(
        ready
            .get("base_url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    )
    .unwrap();
    let mut connection =
        TcpStream::connect((Ipv4Addr::LOCALHOST, base.port().unwrap_or_default())).unwrap();
    connection.set_read_timeout(Some(HANG_TIMEOUT)).unwrap();
    connection
        .write_all(b"GET /blocked HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    connection.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 404"));
    drop(service.child.stdin.take());
    let stopped = Instant::now();
    loop {
        if let Some(status) = service.child.try_wait().unwrap() {
            assert!(status.success(), "proxy failed to shut down");
            break;
        }
        assert!(
            stopped.elapsed() < HANG_TIMEOUT,
            "proxy did not flush on EOF"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let metrics: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("metrics.json")).unwrap()).unwrap();
    assert!(metrics.get("requests").and_then(serde_json::Value::as_u64) == Some(1));
    assert!(!state.join("ready.json").exists());
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("identity.json")).unwrap()).unwrap();
    assert!(identity.get("credential_fingerprint").is_none());
    assert!(
        identity
            .get("transport_revision")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
    );
}
