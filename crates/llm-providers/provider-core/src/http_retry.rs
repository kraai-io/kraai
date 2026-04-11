use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::header::RETRY_AFTER;
use reqwest::{Response, StatusCode};
use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpRetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
}

impl HttpRetryPolicy {
    pub fn backoff_for_retry(&self, retry_number: u32) -> Duration {
        let exponent = retry_number.saturating_sub(1);
        let multiplier = if exponent >= 31 {
            u32::MAX
        } else {
            1u32 << exponent
        };

        self.initial_backoff
            .checked_mul(multiplier)
            .unwrap_or(Duration::MAX)
    }
}

pub const DEFAULT_HTTP_RETRY_POLICY: HttpRetryPolicy = HttpRetryPolicy {
    max_attempts: 20,
    initial_backoff: Duration::from_secs(1),
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRetryEvent {
    pub operation: &'static str,
    pub retry_number: u32,
    pub delay: Duration,
    pub reason: String,
}

pub trait ProviderRetryObserver: Send + Sync {
    fn on_retry_scheduled(&self, event: &ProviderRetryEvent);
}

#[derive(Clone, Default)]
pub struct ProviderRequestContext {
    retry_observer: Option<Arc<dyn ProviderRetryObserver>>,
}

impl ProviderRequestContext {
    pub fn new(retry_observer: Option<Arc<dyn ProviderRetryObserver>>) -> Self {
        Self { retry_observer }
    }

    pub fn with_retry_observer(retry_observer: Arc<dyn ProviderRetryObserver>) -> Self {
        Self {
            retry_observer: Some(retry_observer),
        }
    }

    pub fn retry_observer(&self) -> Option<&dyn ProviderRetryObserver> {
        self.retry_observer.as_deref()
    }
}

pub async fn send_with_retry<F, Fut>(
    operation: &'static str,
    policy: &HttpRetryPolicy,
    request_context: &ProviderRequestContext,
    mut send: F,
) -> reqwest::Result<Response>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<Response>>,
{
    for attempt_number in 1..=policy.max_attempts {
        match send().await {
            Ok(response) => {
                if !is_retryable_status(response.status()) || attempt_number >= policy.max_attempts
                {
                    return Ok(response);
                }

                let retry_number = attempt_number;
                let reason = format!("HTTP {}", response.status());
                let delay = retry_delay_from_response(policy, retry_number, &response);
                notify_retry(
                    request_context,
                    operation,
                    retry_number,
                    delay,
                    reason.clone(),
                );
                warn!(
                    operation,
                    retry_number,
                    delay_seconds = delay.as_secs(),
                    reason,
                    "Retrying provider HTTP request after retryable response",
                );
                tokio::time::sleep(delay).await;
            }
            Err(error) => {
                if !is_retryable_error(&error) || attempt_number >= policy.max_attempts {
                    return Err(error);
                }

                let retry_number = attempt_number;
                let delay = policy.backoff_for_retry(retry_number);
                let reason = error.to_string();
                notify_retry(
                    request_context,
                    operation,
                    retry_number,
                    delay,
                    reason.clone(),
                );
                warn!(
                    operation,
                    retry_number,
                    delay_seconds = delay.as_secs(),
                    reason,
                    "Retrying provider HTTP request after transport failure",
                );
                tokio::time::sleep(delay).await;
            }
        }
    }

    unreachable!("HTTP retry policy must allow at least one attempt")
}

fn notify_retry(
    request_context: &ProviderRequestContext,
    operation: &'static str,
    retry_number: u32,
    delay: Duration,
    reason: String,
) {
    if let Some(observer) = request_context.retry_observer() {
        observer.on_retry_scheduled(&ProviderRetryEvent {
            operation,
            retry_number,
            delay,
            reason,
        });
    }
}

fn retry_delay_from_response(
    policy: &HttpRetryPolicy,
    retry_number: u32,
    response: &Response,
) -> Duration {
    parse_retry_after(response).unwrap_or_else(|| policy.backoff_for_retry(retry_number))
}

fn parse_retry_after(response: &Response) -> Option<Duration> {
    let header = response.headers().get(RETRY_AFTER)?;
    let raw = header.to_str().ok()?.trim();
    if raw.is_empty() {
        return None;
    }

    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = httpdate::parse_http_date(raw).ok()?;
    Some(
        retry_at
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::CONFLICT | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

fn is_retryable_error(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error.is_connect()
        || (error.is_request()
            && error.status().is_none()
            && !error.is_body()
            && !error.is_decode()
            && !error.is_builder()
            && !error.is_redirect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::net::{SocketAddr, TcpListener as StdTcpListener};
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct RetryCollector {
        events: Arc<Mutex<Vec<ProviderRetryEvent>>>,
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

    async fn spawn_server(script: Vec<ScriptedResponse>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
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
                        write_response(&mut stream, status_line, &headers, body).await;
                    }
                    ScriptedResponse::DelayedStatus {
                        delay,
                        status_line,
                        headers,
                        body,
                    } => {
                        tokio::time::sleep(delay).await;
                        write_response(&mut stream, status_line, &headers, body).await;
                    }
                }
            }
        });

        address
    }

    async fn write_response(
        stream: &mut tokio::net::TcpStream,
        status_line: &str,
        headers: &[(&str, String)],
        body: &str,
    ) {
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

        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    }

    fn test_policy() -> HttpRetryPolicy {
        HttpRetryPolicy {
            max_attempts: 20,
            initial_backoff: Duration::from_millis(5),
        }
    }

    fn closed_port_url() -> String {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}/")
    }

    #[tokio::test]
    async fn retries_500_then_succeeds() {
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
        .await;
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "test",
            &test_policy(),
            &ProviderRequestContext::default(),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn retries_429_then_succeeds() {
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
        .await;
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "test",
            &test_policy(),
            &ProviderRequestContext::default(),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn retries_408_then_succeeds() {
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
        .await;
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "test",
            &test_policy(),
            &ProviderRequestContext::default(),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn retries_409_then_succeeds() {
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
        .await;
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "test",
            &test_policy(),
            &ProviderRequestContext::default(),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn does_not_retry_400() {
        let address = spawn_server(vec![ScriptedResponse::Status {
            status_line: "400 Bad Request",
            headers: Vec::new(),
            body: "bad request",
        }])
        .await;
        let collector = Arc::new(RetryCollector::default());
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "test",
            &test_policy(),
            &ProviderRequestContext::with_retry_observer(collector.clone()),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(collector.snapshot().is_empty());
    }

    #[tokio::test]
    async fn does_not_retry_401() {
        let address = spawn_server(vec![ScriptedResponse::Status {
            status_line: "401 Unauthorized",
            headers: Vec::new(),
            body: "nope",
        }])
        .await;
        let collector = Arc::new(RetryCollector::default());
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "test",
            &test_policy(),
            &ProviderRequestContext::with_retry_observer(collector.clone()),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(collector.snapshot().is_empty());
    }

    #[tokio::test]
    async fn retries_transport_connect_failure() {
        let ok_address = spawn_server(vec![ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        }])
        .await;
        let closed_url = closed_port_url();
        let ok_url = format!("http://{ok_address}/");
        let client = reqwest::Client::new();
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
                    if current == 0 {
                        closed_url.clone()
                    } else {
                        ok_url.clone()
                    }
                };
                client.get(url).send()
            },
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn retries_transport_timeout() {
        let slow_address = spawn_server(vec![ScriptedResponse::DelayedStatus {
            delay: Duration::from_millis(50),
            status_line: "200 OK",
            headers: Vec::new(),
            body: "slow",
        }])
        .await;
        let ok_address = spawn_server(vec![ScriptedResponse::Status {
            status_line: "200 OK",
            headers: Vec::new(),
            body: "ok",
        }])
        .await;
        let slow_url = format!("http://{slow_address}/");
        let ok_url = format!("http://{ok_address}/");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(10))
            .build()
            .unwrap();
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
                    if current == 0 {
                        slow_url.clone()
                    } else {
                        ok_url.clone()
                    }
                };
                client.get(url).send()
            },
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn stops_after_max_attempts() {
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
        .await;
        let policy = HttpRetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
        };
        let collector = Arc::new(RetryCollector::default());
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "test",
            &policy,
            &ProviderRequestContext::with_retry_observer(collector.clone()),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(collector.snapshot().len(), 2);
    }

    #[tokio::test]
    async fn emits_retry_events_with_exact_number_and_delay() {
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
        .await;
        let policy = HttpRetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(7),
        };
        let collector = Arc::new(RetryCollector::default());
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "responses",
            &policy,
            &ProviderRequestContext::with_retry_observer(collector.clone()),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let events = collector.snapshot();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].operation, "responses");
        assert_eq!(events[0].retry_number, 1);
        assert_eq!(events[0].delay, Duration::from_millis(7));
        assert_eq!(events[1].retry_number, 2);
        assert_eq!(events[1].delay, Duration::from_millis(14));
    }

    #[tokio::test]
    async fn uses_retry_after_when_parseable() {
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
        .await;
        let collector = Arc::new(RetryCollector::default());
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "responses",
            &test_policy(),
            &ProviderRequestContext::with_retry_observer(collector.clone()),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(collector.snapshot()[0].delay, Duration::ZERO);
    }

    #[tokio::test]
    async fn falls_back_when_retry_after_is_invalid() {
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
        .await;
        let collector = Arc::new(RetryCollector::default());
        let policy = HttpRetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(11),
        };
        let client = reqwest::Client::new();

        let response = send_with_retry(
            "responses",
            &policy,
            &ProviderRequestContext::with_retry_observer(collector.clone()),
            || client.get(format!("http://{address}/")).send(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(collector.snapshot()[0].delay, Duration::from_millis(11));
    }
}
