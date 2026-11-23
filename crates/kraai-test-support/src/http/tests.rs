use super::*;
use tokio::io::AsyncReadExt;

async fn connect(address: SocketAddr, request: String) -> io::Result<TcpStream> {
    let mut client = TcpStream::connect(address).await?;
    for chunk in request.as_bytes().chunks(3) {
        client.write_all(chunk).await?;
        tokio::task::yield_now().await;
    }
    Ok(client)
}

#[tokio::test]
async fn stray_requests_get_404_and_the_expected_one_is_left_unread() {
    let mut listener = Listener::bind(PATH).await.unwrap();
    let address = listener.local_addr().unwrap();
    let request = format!("POST {PATH}/chat?x=1 HTTP/1.1\r\nContent-Length: 2\r\n\r\n{{}}");
    let sent = request.clone();
    let clients = tokio::spawn(async move {
        let mut stray =
            connect(address, "GET / HTTP/1.1\r\nUser-Agent: node\r\n\r\n".into()).await?;
        let mut response = Vec::new();
        stray.read_to_end(&mut response).await?;
        let expected = connect(address, sent).await?;
        Ok::<_, io::Error>((response, expected))
    });
    let (mut stream, _) = crate::hang_guard(listener.accept()).await.unwrap().unwrap();
    let mut received = vec![0; request.len()];
    stream.read_exact(&mut received).await.unwrap();
    assert_eq!(received, request.as_bytes());
    let (response, _expected) = clients.await.unwrap().unwrap();
    assert!(response.starts_with(b"HTTP/1.1 404"));
}

#[tokio::test]
async fn silent_connections_do_not_block_the_expected_one() {
    let mut listener = Listener::bind(PATH).await.unwrap();
    let address = listener.local_addr().unwrap();
    let _silent = TcpStream::connect(address).await.unwrap();
    let _client = connect(address, format!("GET {PATH} HTTP/1.1\r\n\r\n"))
        .await
        .unwrap();
    crate::hang_guard(listener.accept()).await.unwrap().unwrap();
}

#[test]
fn received_ignores_requests_outside_the_prefix() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let mut stray = std::net::TcpStream::connect(address).unwrap();
    stray.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
    assert!(!received(&listener, PATH).unwrap());
    let mut leaked = std::net::TcpStream::connect(address).unwrap();
    leaked
        .write_all(format!("GET {PATH} HTTP/1.1\r\n\r\n").as_bytes())
        .unwrap();
    assert!(received(&listener, PATH).unwrap());
}
