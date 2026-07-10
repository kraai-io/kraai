use std::time::Duration;

use reqwest::{Client, ClientBuilder, RequestBuilder};

pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const HTTP_FINITE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const HTTP_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Build a client suitable for long-lived streams.
///
/// The read timeout resets whenever bytes arrive, so healthy streams may live
/// indefinitely while stalled response bodies are bounded.
pub fn build_streaming_http_client() -> reqwest::Result<Client> {
    base_client_builder(HTTP_CONNECT_TIMEOUT, HTTP_STREAM_IDLE_TIMEOUT).build()
}

/// Build a client for authentication and other finite JSON requests.
pub fn build_finite_http_client() -> reqwest::Result<Client> {
    base_client_builder(HTTP_CONNECT_TIMEOUT, HTTP_STREAM_IDLE_TIMEOUT)
        .timeout(HTTP_FINITE_REQUEST_TIMEOUT)
        .build()
}

/// Apply the finite-operation deadline to a request made by a streaming client.
pub fn finite_request(builder: RequestBuilder) -> RequestBuilder {
    builder.timeout(HTTP_FINITE_REQUEST_TIMEOUT)
}

fn base_client_builder(connect_timeout: Duration, read_timeout: Duration) -> ClientBuilder {
    Client::builder()
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "HTTP timeout tests use direct local fixture assertions"
)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn client_or_skip(builder: ClientBuilder) -> Option<Client> {
        match builder.build() {
            Ok(client) => Some(client),
            Err(error)
                if error
                    .to_string()
                    .contains("No CA certificates were loaded from the system") =>
            {
                None
            }
            Err(error) => panic!("unexpected client build failure: {error}"),
        }
    }

    #[tokio::test]
    async fn finite_client_times_out_waiting_for_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let Some(client) = client_or_skip(
            base_client_builder(Duration::from_millis(100), Duration::from_millis(100))
                .timeout(Duration::from_millis(25)),
        ) else {
            return;
        };

        let error = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap_err();

        assert!(error.is_timeout());
    }

    #[tokio::test]
    async fn streaming_client_times_out_when_body_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n",
                )
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let Some(client) = client_or_skip(base_client_builder(
            Duration::from_millis(100),
            Duration::from_millis(25),
        )) else {
            return;
        };

        let response = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let mut body = response.bytes_stream();
        assert_eq!(body.next().await.unwrap().unwrap().as_ref(), b"hello");
        let error = body.next().await.unwrap().unwrap_err();

        assert!(error.is_timeout());
    }
}
