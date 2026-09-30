use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn http_client_is_initialized_for_scripts_and_inherited_startup() {
    for startup in [NushellStartup::Clean, NushellStartup::Inherit] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind HTTP fixture");
        let address = listener.local_addr().expect("HTTP fixture address");
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(10), async move {
                let (mut stream, _) = listener.accept().await.expect("accept HTTP request");
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    assert!(request.len() < 8192, "oversized HTTP request");
                    assert_ne!(stream.read_buf(&mut request).await.expect("read HTTP request"), 0);
                }
                assert!(request.starts_with(b"GET / HTTP/1.1\r\n"));
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\nhttp-ok\n")
                    .await.expect("write HTTP response");
            }).await.expect("HTTP fixture timed out");
        });
        let workspace = TestWorkspace::new();
        let fetch = format!("http get --max-time 5sec http://{address}");
        let mut execution = if startup == NushellStartup::Inherit {
            let config_home = workspace.0.join("config");
            let nushell_config = config_home.join("nushell");
            std::fs::create_dir_all(&nushell_config).expect("create config directory");
            std::fs::write(
                nushell_config.join("env.nu"),
                format!("$env.HTTP_RESULT = ({fetch})"),
            )
            .expect("write HTTP startup config");
            let mut execution = plan(b"print -n $env.HTTP_RESULT".to_vec(), &workspace);
            execution
                .environment
                .insert("XDG_CONFIG_HOME".into(), config_home.display().to_string());
            execution
        } else {
            plan(format!("print -n ({fetch})").into_bytes(), &workspace)
        };
        execution.nushell_startup = startup;
        let result = execute(execution, CancellationToken::new())
            .await
            .expect("run HTTP script");
        if result.output.termination != (Termination::Exited { code: Some(0) }) {
            server.abort();
        }
        assert_eq!(
            result.output.termination,
            Termination::Exited { code: Some(0) },
            "{}",
            String::from_utf8_lossy(&result.output.stderr)
        );
        server.await.expect("HTTP fixture panicked");
        assert_eq!(String::from_utf8_lossy(&result.output.stdout), "http-ok\n");
        assert!(
            result.output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&result.output.stderr)
        );
    }
}
