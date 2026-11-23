use super::*;
use kraai_test_support::http;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn anonymous_request_and_http_failures() {
    for (status, body, expected) in [
        (
            "200 OK",
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"result"}]}}"#,
            "result",
        ),
        ("429 Too Many Requests", "", "rate limited"),
        ("401 Unauthorized", "", "anonymous access"),
        ("503 Unavailable", "", "HTTP error"),
    ] {
        let mut listener = http::Listener::bind(http::PATH).await.unwrap();
        let endpoint = listener.url().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut request = vec![0; 8192];
            let read = stream.read(&mut request).await?;
            request.truncate(read);
            let request = String::from_utf8_lossy(&request).to_lowercase();
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("apikey"));
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let request = WebSearchRequest {
            query: String::from("test"),
            limit: 5,
            max_chars: 6000,
        };
        let result = search(&client, &endpoint, &request).await;
        server.await.unwrap().unwrap();
        let text = result
            .map(|result| result.content)
            .unwrap_or_else(|error| error);
        assert!(text.contains(expected), "{text}");
    }
}

#[tokio::test]
async fn oversized_stream_is_rejected_before_eof() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut listener = http::Listener::bind(http::PATH).await.unwrap();
    let endpoint = listener.url().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = vec![0; 8192];
        if stream.read(&mut request).await? == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
            .await?;
        stream.write_all(&vec![b'x'; MAX_BYTES + 1]).await?;
        std::future::pending::<std::io::Result<()>>().await
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let request = WebSearchRequest {
        query: String::from("test"),
        limit: 5,
        max_chars: 6000,
    };
    let result = kraai_test_support::hang_guard(search(&client, &endpoint, &request)).await;
    server.abort();
    match result.unwrap() {
        Err(error) if error.contains("exceeds 1 MiB") => {}
        other => panic!("unexpected result: {other:?}"),
    }
}
