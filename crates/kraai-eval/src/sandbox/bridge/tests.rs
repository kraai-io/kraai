use std::os::unix::process::ExitStatusExt;
use std::process::{ExitCode, ExitStatus};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener};

use super::forwarding::{exit_code, forward_connection, forward_connections};

#[tokio::test]
async fn forwards_loopback_clients_to_the_proxy_socket() {
    let root = std::env::temp_dir().join(format!("kraai-eval-bridge-{}", ulid::Ulid::generate()));
    std::fs::create_dir(&root).unwrap();
    let socket = root.join("proxy.sock");
    let upstream = UnixListener::bind(&socket).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let forwarder = tokio::spawn(forward_connections(listener, socket));
    let exchange = async {
        let upstream = async {
            let (mut stream, _) = upstream.accept().await?;
            let mut request = Vec::new();
            stream.read_to_end(&mut request).await?;
            stream.write_all(b"pong:").await?;
            stream.write_all(&request).await?;
            stream.shutdown().await?;
            Ok::<_, color_eyre::Report>(())
        };
        let client = async {
            let mut stream = TcpStream::connect(address).await?;
            stream.write_all(b"ping").await?;
            stream.shutdown().await?;
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await?;
            Ok::<_, color_eyre::Report>(response)
        };
        let (upstream, client) = tokio::join!(upstream, client);
        upstream?;
        client
    };
    let response = kraai_test_support::hang_guard(exchange)
        .await
        .unwrap()
        .unwrap();
    forwarder.abort();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        response == b"pong:ping",
        "unexpected bridged response {response:?}"
    );
}

#[tokio::test]
async fn relay_ends_when_the_proxy_closes_on_a_lingering_client() {
    let root = std::env::temp_dir().join(format!("kraai-eval-bridge-{}", ulid::Ulid::generate()));
    std::fs::create_dir(&root).unwrap();
    let socket = root.join("proxy.sock");
    let upstream = UnixListener::bind(&socket).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (accepted, _) = listener.accept().await.unwrap();
    let exchange = async {
        let proxy = async {
            let (mut stream, _) = upstream.accept().await?;
            stream.write_all(b"done").await?;
            Ok::<_, color_eyre::Report>(())
        };
        let (relay, proxy) = tokio::join!(forward_connection(accepted, &socket), proxy);
        relay?;
        proxy?;
        let mut response = Vec::new();
        client.read_to_end(&mut response).await?;
        Ok::<_, color_eyre::Report>(response)
    };
    let response = kraai_test_support::hang_guard(exchange)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        response == b"done",
        "unexpected bridged response {response:?}"
    );
}

#[test]
fn exit_status_survives_the_bridge() {
    assert_eq!(
        exit_code(ExitStatus::from_raw(7 << 8)),
        ExitCode::from(7),
        "exit codes must pass through"
    );
    assert_eq!(
        exit_code(ExitStatus::from_raw(9)),
        ExitCode::from(137),
        "signals must map to 128 + signal"
    );
}
