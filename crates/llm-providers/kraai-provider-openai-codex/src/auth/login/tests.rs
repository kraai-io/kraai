use kraai_test_support::HANG_TIMEOUT;

use super::*;

#[tokio::test]
async fn callback_listener_prefers_primary_registered_port() {
    let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary = reservation.local_addr().unwrap().port();
    drop(reservation);

    let listener = bind_listener(primary, &[]).await.unwrap();

    assert_eq!(listener.local_addr().unwrap().port(), primary);
}

#[tokio::test]
async fn callback_listener_uses_registered_fallback() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary = primary_listener.local_addr().unwrap().port();
    let fallback_reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback = fallback_reservation.local_addr().unwrap().port();
    drop(fallback_reservation);

    let listener = bind_listener(primary, &[fallback]).await.unwrap();

    assert_eq!(listener.local_addr().unwrap().port(), fallback);
}

#[tokio::test]
async fn callback_listener_fails_when_registered_ports_are_occupied() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary = primary_listener.local_addr().unwrap().port();
    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback = fallback_listener.local_addr().unwrap().port();

    let error = bind_listener(primary, &[fallback]).await.unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
    assert!(error.to_string().contains(&primary.to_string()));
    assert!(error.to_string().contains(&fallback.to_string()));
}

#[tokio::test]
async fn callback_request_reader_accepts_fragmented_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let writer = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        for chunk in [
            b"GET /auth/call".as_slice(),
            b"back?code=test&state=ok HTTP/1.1\r\nHost: local".as_slice(),
            b"host\r\n\r".as_slice(),
            b"\n".as_slice(),
        ] {
            client.write_all(chunk).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    let (mut server, _) = listener.accept().await.unwrap();

    let request = read_http_request(&mut server, HANG_TIMEOUT).await.unwrap();
    writer.await.unwrap();

    assert!(request.starts_with("GET /auth/callback?code=test&state=ok HTTP/1.1"));
    assert!(request.ends_with("\r\n\r\n"));
}

#[tokio::test]
async fn callback_request_reader_rejects_oversized_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let writer = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(&vec![b'x'; MAX_CALLBACK_HEADER_BYTES + 1])
            .await
            .unwrap();
    });
    let (mut server, _) = listener.accept().await.unwrap();

    let error = read_http_request(&mut server, HANG_TIMEOUT)
        .await
        .unwrap_err();
    writer.await.unwrap();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn callback_request_reader_times_out_idle_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = tokio::spawn(tokio::net::TcpStream::connect(address));
    let (mut server, _) = listener.accept().await.unwrap();
    let _client = client.await.unwrap().unwrap();

    let error = read_http_request(&mut server, CALLBACK_REQUEST_TIMEOUT)
        .await
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}
