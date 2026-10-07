#![expect(
    clippy::unwrap_used,
    reason = "HTTP fixtures assert local transport operations directly"
)]

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn response(wire: &'static [u8]) -> Response {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await.unwrap();
        stream.write_all(wire).await.unwrap();
    });
    client_builder(HttpTimeouts::FINITE, reqwest::redirect::Policy::none())
        .tls_certs_only([])
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}/"))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn bounded_body_rejects_advertised_and_chunked_overflow() {
    for wire in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nhe\r\n3\r\nllo\r\n0\r\n\r\n"[..],
    ] {
        assert!(matches!(read_response_body(response(wire).await, 4).await,
            Err(BodyReadError::TooLarge { limit: 4 })));
    }
}

#[tokio::test]
async fn bounded_body_accepts_exact_limit_and_rejects_truncated_transport() {
    let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
    assert_eq!(
        read_response_body(response(wire).await, 5).await.unwrap(),
        b"hello"
    );
    let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhi";
    assert!(matches!(
        read_response_body(response(wire).await, 5).await,
        Err(BodyReadError::Transport(_))
    ));
}

#[tokio::test]
async fn prefix_marks_overflow_without_confusing_exact_limit() {
    let wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nhe\r\n3\r\nllo\r\n0\r\n\r\n";
    assert_eq!(
        read_response_prefix(response(wire).await, 4).await.unwrap(),
        ReadPrefix {
            bytes: b"hell".to_vec(),
            truncated: true,
        }
    );
    assert_eq!(
        read_response_prefix(response(wire).await, 5).await.unwrap(),
        ReadPrefix {
            bytes: b"hello".to_vec(),
            truncated: false,
        }
    );
    assert_eq!(
        read_response_prefix(response(wire).await, 0).await.unwrap(),
        ReadPrefix {
            bytes: Vec::new(),
            truncated: true,
        }
    );
}

#[tokio::test]
async fn redirect_policy_is_explicit() {
    let response = response(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
}

#[tokio::test]
async fn text_prefix_preserves_charset_and_bom_decoding() {
    let wire = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=iso-8859-1\r\nContent-Length: 4\r\nConnection: close\r\n\r\ncaf\xe9";
    assert_eq!(
        read_response_text_prefix(response(wire).await, 4)
            .await
            .unwrap(),
        TextPrefix {
            text: "café".into(),
            truncated: false,
        }
    );
    let wire = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 5\r\nConnection: close\r\n\r\n\xef\xbb\xbf{}";
    assert_eq!(
        read_response_text_prefix(response(wire).await, 5)
            .await
            .unwrap()
            .text,
        "{}"
    );
}
