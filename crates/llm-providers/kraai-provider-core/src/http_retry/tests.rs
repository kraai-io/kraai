use super::*;
use color_eyre::{Result, eyre::eyre};
use std::collections::VecDeque;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::request_context::ProviderRetryObserver;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn is_missing_system_ca_error(error: &dyn std::error::Error) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        let display = error.to_string();
        let debug = format!("{error:?}");
        if display.contains("No CA certificates were loaded from the system")
            || debug.contains("No CA certificates were loaded from the system")
            || display == "builder error"
        {
            return true;
        }
        current = error.source();
    }
    false
}

fn test_client_from_builder_or_skip(builder: reqwest::ClientBuilder) -> Option<reqwest::Client> {
    match builder.build() {
        Ok(client) => Some(client),
        Err(error) if is_missing_system_ca_error(&error) => None,
        Err(error) => panic!("unexpected reqwest client build error: {error}"),
    }
}

fn test_client_or_skip() -> Option<reqwest::Client> {
    test_client_from_builder_or_skip(reqwest::Client::builder())
}

#[derive(Clone, Default)]
struct RetryCollector {
    events: Arc<Mutex<Vec<ProviderRetryEvent>>>,
    attempts: Arc<Mutex<Vec<u32>>>,
}

impl RetryCollector {
    fn snapshot(&self) -> Vec<ProviderRetryEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl ProviderRetryObserver for RetryCollector {
    fn before_attempt(
        &self,
        unpriced_prior_attempts: u32,
    ) -> Pin<Box<dyn Future<Output = color_eyre::Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.attempts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(unpriced_prior_attempts);
            Ok(())
        })
    }
    fn on_retry_scheduled(&self, event: &ProviderRetryEvent) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event.clone());
    }
}

enum ScriptedResponse {
    Status {
        status_line: &'static str,
        headers: Vec<(&'static str, String)>,
        body: &'static str,
    },
    DelayedStatus {
        delay: Duration,
        status_line: &'static str,
        headers: Vec<(&'static str, String)>,
        body: &'static str,
    },
}

async fn spawn_server(script: Vec<ScriptedResponse>) -> Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let script = Arc::new(tokio::sync::Mutex::new(VecDeque::from(script)));

    tokio::spawn(async move {
        loop {
            let accept_result = listener.accept().await;
            let Ok((mut stream, _)) = accept_result else {
                break;
            };

            let next = {
                let mut guard = script.lock().await;
                guard.pop_front()
            };
            let Some(next) = next else {
                break;
            };

            let mut buffer = [0_u8; 2048];
            let _ = stream.read(&mut buffer).await;

            match next {
                ScriptedResponse::Status {
                    status_line,
                    headers,
                    body,
                } => {
                    let _ = write_response(&mut stream, status_line, &headers, body).await;
                }
                ScriptedResponse::DelayedStatus {
                    delay,
                    status_line,
                    headers,
                    body,
                } => {
                    tokio::time::sleep(delay).await;
                    let _ = write_response(&mut stream, status_line, &headers, body).await;
                }
            }
        }
    });

    Ok(address)
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status_line: &str,
    headers: &[(&str, String)],
    body: &str,
) -> std::io::Result<()> {
    let mut response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    response.push_str(body);

    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

fn test_policy() -> HttpRetryPolicy {
    HttpRetryPolicy {
        max_attempts: 20,
        initial_backoff: Duration::from_millis(5),
        max_delay: Duration::from_millis(50),
        max_elapsed: Duration::from_secs(2),
    }
}

fn closed_port_url() -> std::io::Result<String> {
    let listener = StdTcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(format!("http://{address}/"))
}

#[tokio::test]
async fn failed_attempt_record_prevents_retry_send() -> Result<()> {
    struct RejectRetry;
    impl ProviderRetryObserver for RejectRetry {
        fn on_retry_scheduled(&self, _event: &ProviderRetryEvent) {}
        fn before_attempt(
            &self,
            unpriced_prior_attempts: u32,
        ) -> Pin<Box<dyn Future<Output = color_eyre::Result<()>> + Send + '_>> {
            Box::pin(async move {
                if unpriced_prior_attempts > 0 {
                    return Err(eyre!("receipt unavailable"));
                }
                Ok(())
            })
        }
    }
    let address = spawn_server(vec![ScriptedResponse::Status {
        status_line: "500 Internal Server Error",
        headers: Vec::new(),
        body: "try again",
    }])
    .await?;
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };
    let context = ProviderRequestContext::with_retry_observer(Arc::new(RejectRetry));
    let mut sends = 0;
    let result = send_with_retry("test", &test_policy(), &context, || {
        sends += 1;
        client.get(format!("http://{address}/")).send()
    })
    .await;
    assert_eq!(sends, 1);
    assert!(result.is_err_and(|error| error.to_string() == "receipt unavailable"));
    Ok(())
}

#[tokio::test]
async fn retries_500_then_succeeds() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "500 Internal Server Error",
            headers: Vec::new(),
            body: "try again",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        },
    ])
    .await?;
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let observer = Arc::new(RetryCollector::default());
    let context = ProviderRequestContext::with_retry_observer(observer.clone());
    let response = send_with_retry("test", &test_policy(), &context, || {
        client.get(format!("http://{address}/")).send()
    })
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        *observer
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        [0, 1]
    );
    Ok(())
}

#[tokio::test]
async fn retries_429_then_succeeds() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "429 Too Many Requests",
            headers: Vec::new(),
            body: "slow down",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        },
    ])
    .await?;
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let observer = Arc::new(RetryCollector::default());
    let context = ProviderRequestContext::with_retry_observer(observer.clone());
    let response = send_with_retry("test", &test_policy(), &context, || {
        client.get(format!("http://{address}/")).send()
    })
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        *observer
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        [0, 0]
    );
    Ok(())
}

#[tokio::test]
async fn retries_408_then_succeeds() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "408 Request Timeout",
            headers: Vec::new(),
            body: "timeout",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        },
    ])
    .await?;
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let observer = Arc::new(RetryCollector::default());
    let context = ProviderRequestContext::with_retry_observer(observer.clone());
    let response = send_with_retry("test", &test_policy(), &context, || {
        client.get(format!("http://{address}/")).send()
    })
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        *observer
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        [0, 1]
    );
    Ok(())
}

#[tokio::test]
async fn retries_409_then_succeeds() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "409 Conflict",
            headers: Vec::new(),
            body: "conflict",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        },
    ])
    .await?;
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let response = send_with_retry(
        "test",
        &test_policy(),
        &ProviderRequestContext::default(),
        || client.get(format!("http://{address}/")).send(),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn does_not_retry_400() -> Result<()> {
    let address = spawn_server(vec![ScriptedResponse::Status {
        status_line: "400 Bad Request",
        headers: Vec::new(),
        body: "bad request",
    }])
    .await?;
    let collector = Arc::new(RetryCollector::default());
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let response = send_with_retry(
        "test",
        &test_policy(),
        &ProviderRequestContext::with_retry_observer(collector.clone()),
        || client.get(format!("http://{address}/")).send(),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(collector.snapshot().is_empty());
    Ok(())
}

#[tokio::test]
async fn does_not_retry_401() -> Result<()> {
    let address = spawn_server(vec![ScriptedResponse::Status {
        status_line: "401 Unauthorized",
        headers: Vec::new(),
        body: "nope",
    }])
    .await?;
    let collector = Arc::new(RetryCollector::default());
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let response = send_with_retry(
        "test",
        &test_policy(),
        &ProviderRequestContext::with_retry_observer(collector.clone()),
        || client.get(format!("http://{address}/")).send(),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(collector.snapshot().is_empty());
    Ok(())
}

#[tokio::test]
async fn retries_transport_connect_failure() -> Result<()> {
    let ok_address = spawn_server(vec![ScriptedResponse::Status {
        status_line: "200 OK",
        headers: Vec::new(),
        body: "ok",
    }])
    .await?;
    let closed_url = closed_port_url()?;
    let ok_url = format!("http://{ok_address}/");
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };
    let attempt = Arc::new(Mutex::new(0usize));

    let response = send_with_retry(
        "test",
        &test_policy(),
        &ProviderRequestContext::default(),
        || {
            let attempt = attempt.clone();
            let url = {
                let mut guard = attempt
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let current = *guard;
                *guard += 1;
                drop(guard);
                if current == 0 {
                    closed_url.clone()
                } else {
                    ok_url.clone()
                }
            };
            client.get(url).send()
        },
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn retries_transport_timeout() -> Result<()> {
    let slow_address = spawn_server(vec![ScriptedResponse::DelayedStatus {
        delay: Duration::from_millis(50),
        status_line: "200 OK",
        headers: Vec::new(),
        body: "slow",
    }])
    .await?;
    let ok_address = spawn_server(vec![ScriptedResponse::Status {
        status_line: "200 OK",
        headers: Vec::new(),
        body: "ok",
    }])
    .await?;
    let slow_url = format!("http://{slow_address}/");
    let ok_url = format!("http://{ok_address}/");
    let Some(client) = test_client_from_builder_or_skip(
        reqwest::Client::builder().timeout(Duration::from_millis(10)),
    ) else {
        return Ok(());
    };
    let attempt = Arc::new(Mutex::new(0usize));

    let response = send_with_retry(
        "test",
        &test_policy(),
        &ProviderRequestContext::default(),
        || {
            let attempt = attempt.clone();
            let url = {
                let mut guard = attempt
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let current = *guard;
                *guard += 1;
                drop(guard);
                if current == 0 {
                    slow_url.clone()
                } else {
                    ok_url.clone()
                }
            };
            client.get(url).send()
        },
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn stops_after_max_attempts() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "500 Internal Server Error",
            headers: Vec::new(),
            body: "1",
        },
        ScriptedResponse::Status {
            status_line: "500 Internal Server Error",
            headers: Vec::new(),
            body: "2",
        },
        ScriptedResponse::Status {
            status_line: "500 Internal Server Error",
            headers: Vec::new(),
            body: "3",
        },
    ])
    .await?;
    let policy = HttpRetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(1),
        max_delay: Duration::from_millis(10),
        max_elapsed: Duration::from_secs(1),
    };
    let collector = Arc::new(RetryCollector::default());
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let response = send_with_retry(
        "test",
        &policy,
        &ProviderRequestContext::with_retry_observer(collector.clone()),
        || client.get(format!("http://{address}/")).send(),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(collector.snapshot().len(), 2);
    Ok(())
}

#[tokio::test]
async fn emits_retry_events_with_exact_number_and_delay() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "500 Internal Server Error",
            headers: Vec::new(),
            body: "1",
        },
        ScriptedResponse::Status {
            status_line: "500 Internal Server Error",
            headers: Vec::new(),
            body: "2",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        },
    ])
    .await?;
    let policy = HttpRetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(7),
        max_delay: Duration::from_millis(20),
        max_elapsed: Duration::from_secs(1),
    };
    let collector = Arc::new(RetryCollector::default());
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let response = send_with_retry(
        "responses",
        &policy,
        &ProviderRequestContext::with_retry_observer(collector.clone()),
        || client.get(format!("http://{address}/")).send(),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);

    let events = collector.snapshot();
    assert_eq!(events.len(), 2);
    let [first, second] = events.as_slice() else {
        return Err(eyre!("expected two retry events"));
    };
    assert_eq!(first.operation, "responses");
    assert_eq!(first.retry_number, 1);
    assert!(first.delay >= Duration::from_micros(5_600));
    assert!(first.delay <= Duration::from_micros(8_400));
    assert_eq!(second.retry_number, 2);
    assert!(second.delay >= Duration::from_micros(11_200));
    assert!(second.delay <= Duration::from_micros(16_800));
    Ok(())
}

#[tokio::test]
async fn uses_retry_after_when_parseable() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "429 Too Many Requests",
            headers: vec![("Retry-After", String::from("0"))],
            body: "wait",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        },
    ])
    .await?;
    let collector = Arc::new(RetryCollector::default());
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let response = send_with_retry(
        "responses",
        &test_policy(),
        &ProviderRequestContext::with_retry_observer(collector.clone()),
        || client.get(format!("http://{address}/")).send(),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collector.snapshot();
    let [event] = events.as_slice() else {
        return Err(eyre!("expected one retry event"));
    };
    assert_eq!(event.delay, Duration::ZERO);
    Ok(())
}

#[tokio::test]
async fn falls_back_when_retry_after_is_invalid() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "503 Service Unavailable",
            headers: vec![("Retry-After", String::from("not-a-date"))],
            body: "wait",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        },
    ])
    .await?;
    let collector = Arc::new(RetryCollector::default());
    let policy = HttpRetryPolicy {
        max_attempts: 2,
        initial_backoff: Duration::from_millis(11),
        max_delay: Duration::from_millis(20),
        max_elapsed: Duration::from_secs(1),
    };
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let response = send_with_retry(
        "responses",
        &policy,
        &ProviderRequestContext::with_retry_observer(collector.clone()),
        || client.get(format!("http://{address}/")).send(),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collector.snapshot();
    let [event] = events.as_slice() else {
        return Err(eyre!("expected one retry event"));
    };
    assert!(event.delay >= Duration::from_micros(8_800));
    assert!(event.delay <= Duration::from_micros(13_200));
    Ok(())
}

#[test]
fn default_retry_policy_is_bounded_and_jitter_stays_within_limits() {
    assert_eq!(DEFAULT_HTTP_RETRY_POLICY.max_attempts, 6);
    assert_eq!(DEFAULT_HTTP_RETRY_POLICY.max_delay, Duration::from_secs(15));
    assert_eq!(
        DEFAULT_HTTP_RETRY_POLICY.max_elapsed,
        Duration::from_secs(60)
    );
    assert_eq!(
        apply_jitter(Duration::from_secs(10), 80),
        Duration::from_secs(8)
    );
    assert_eq!(
        apply_jitter(Duration::from_secs(10), 120),
        Duration::from_secs(12)
    );
    assert!(
        DEFAULT_HTTP_RETRY_POLICY.backoff_for_retry(u32::MAX)
            <= DEFAULT_HTTP_RETRY_POLICY.max_delay
    );
}

#[tokio::test]
async fn clamps_oversized_retry_after_to_max_delay() -> Result<()> {
    let address = spawn_server(vec![
        ScriptedResponse::Status {
            status_line: "429 Too Many Requests",
            headers: vec![("Retry-After", String::from("999999"))],
            body: "wait",
        },
        ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        },
    ])
    .await?;
    let collector = Arc::new(RetryCollector::default());
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };

    let response = send_with_retry(
        "responses",
        &test_policy(),
        &ProviderRequestContext::with_retry_observer(collector.clone()),
        || client.get(format!("http://{address}/")).send(),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collector.snapshot();
    assert_eq!(
        events.first().map(|event| event.delay),
        Some(Duration::from_millis(50))
    );
    Ok(())
}

#[tokio::test]
async fn zero_attempt_policy_is_normalized_to_one_attempt() -> Result<()> {
    let address = spawn_server(vec![ScriptedResponse::Status {
        status_line: "200 OK",
        headers: Vec::new(),
        body: "ok",
    }])
    .await?;
    let Some(client) = test_client_or_skip() else {
        return Ok(());
    };
    let policy = HttpRetryPolicy {
        max_attempts: 0,
        initial_backoff: Duration::ZERO,
        max_delay: Duration::ZERO,
        max_elapsed: Duration::ZERO,
    };

    let response = send_with_retry("test", &policy, &ProviderRequestContext::default(), || {
        client.get(format!("http://{address}/")).send()
    })
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}
