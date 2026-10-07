#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "HTTP timeout tests use direct local fixture assertions"
)]
mod tests {
    use kraai_io::http::{HttpTimeouts, client_builder};
    use reqwest::{Client, ClientBuilder};
    use std::time::Duration;

    fn base_client_builder(connect: Duration, read: Duration) -> ClientBuilder {
        client_builder(
            HttpTimeouts {
                connect: Some(connect),
                read: Some(read),
                request: None,
            },
            reqwest::redirect::Policy::default(),
        )
    }
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn build_test_client(builder: ClientBuilder) -> Client {
        builder
            .tls_certs_only([])
            .build()
            .unwrap_or_else(|error| panic!("unexpected client build failure: {error}"))
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
        let client = build_test_client(
            base_client_builder(Duration::from_millis(100), Duration::from_millis(100))
                .timeout(Duration::from_millis(25)),
        );

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
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nd\r\ndata: hello\n\n\r\n",
                )
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let client = build_test_client(base_client_builder(
            Duration::from_millis(100),
            Duration::from_millis(25),
        ));

        let response = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let mut body = crate::stream_sse_data(response);
        assert_eq!(
            body.next().await.unwrap().unwrap(),
            crate::SseEvent::Data("hello".into())
        );
        let error = body.next().await.unwrap().unwrap_err();

        assert!(matches!(
            error.downcast_ref::<crate::ProviderError>(),
            Some(crate::ProviderError::StreamInterrupted(_))
        ));
        assert!(
            error
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
        );
        assert!(body.next().await.is_none());
    }
}
