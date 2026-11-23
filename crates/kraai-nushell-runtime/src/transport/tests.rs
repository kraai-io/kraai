use super::*;
use std::task::Poll;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn preparation_time_does_not_consume_the_handshake_deadline() {
    let directory = kraai_sandbox::PrivateTempConfig::default()
        .reserve()
        .unwrap();
    let path = directory.path().expect("missing private temp");
    let listener = Listener::bind(path).unwrap();
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    let startup_timeout = kraai_test_support::HANG_TIMEOUT * 2;
    tokio::time::pause();
    let server = accept(listener, spawned_rx, startup_timeout);
    tokio::pin!(server);
    let polled = std::future::poll_fn(|cx| Poll::Ready(server.as_mut().poll(cx))).await;
    if polled.is_ready() {
        panic!("handshake finished before the host was spawned");
    }
    tokio::time::advance(startup_timeout * 2).await;
    spawned_tx
        .send(tokio::time::Instant::now())
        .expect("handshake stopped before spawn");
    tokio::time::resume();
    let client = async {
        let mut stream = tokio::net::UnixStream::connect(path.join("host.sock")).await?;
        stream.write_u8(HOST_READY).await?;
        stream.write_u32(HOST_PROTOCOL_VERSION).await?;
        Ok::<_, io::Error>(stream)
    };
    let (server, client) = kraai_test_support::hang_guard(async { tokio::join!(server, client) })
        .await
        .unwrap();
    let _server = server.unwrap();
    let _client = client.unwrap();
}

#[tokio::test]
async fn custom_startup_timeout_limits_the_handshake() {
    let directory = kraai_sandbox::PrivateTempConfig::default()
        .reserve()
        .unwrap();
    let path = directory.path().expect("missing private temp");
    let listener = Listener::bind(path).unwrap();
    let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
    spawned_tx
        .send(tokio::time::Instant::now())
        .expect("handshake stopped before spawn");
    let startup_timeout = Duration::from_millis(25);
    let result = kraai_test_support::hang_guard(accept(listener, spawned_rx, startup_timeout))
        .await
        .unwrap();
    let error = result.expect_err("missing startup timeout");
    if error.kind() != io::ErrorKind::TimedOut
        || error.to_string() != "Nushell host handshake timed out 25ms after being spawned"
    {
        panic!("unexpected startup timeout: {error}");
    }
}
