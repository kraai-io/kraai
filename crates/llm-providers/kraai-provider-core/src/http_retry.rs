use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use rand::RngExt;
use reqwest::header::RETRY_AFTER;
use reqwest::{Response, StatusCode};
use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpRetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_delay: Duration,
    pub max_elapsed: Duration,
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
            .min(self.max_delay)
    }

    fn jittered_backoff_for_retry(&self, retry_number: u32) -> Duration {
        let percent = rand::rng().random_range(80..=120);
        apply_jitter(self.backoff_for_retry(retry_number), percent).min(self.max_delay)
    }
}

pub const DEFAULT_HTTP_RETRY_POLICY: HttpRetryPolicy = HttpRetryPolicy {
    max_attempts: 6,
    initial_backoff: Duration::from_secs(1),
    max_delay: Duration::from_secs(15),
    max_elapsed: Duration::from_secs(60),
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
    prompt_cache_key: Option<String>,
}

impl ProviderRequestContext {
    pub fn new(retry_observer: Option<Arc<dyn ProviderRetryObserver>>) -> Self {
        Self {
            retry_observer,
            prompt_cache_key: None,
        }
    }

    pub fn with_retry_observer(retry_observer: Arc<dyn ProviderRetryObserver>) -> Self {
        Self {
            retry_observer: Some(retry_observer),
            prompt_cache_key: None,
        }
    }

    pub fn with_prompt_cache_key(prompt_cache_key: String) -> Self {
        Self {
            retry_observer: None,
            prompt_cache_key: Some(prompt_cache_key),
        }
    }

    pub fn with_retry_observer_and_prompt_cache_key(
        retry_observer: Arc<dyn ProviderRetryObserver>,
        prompt_cache_key: String,
    ) -> Self {
        Self {
            retry_observer: Some(retry_observer),
            prompt_cache_key: Some(prompt_cache_key),
        }
    }

    pub fn retry_observer(&self) -> Option<&dyn ProviderRetryObserver> {
        self.retry_observer.as_deref()
    }

    pub fn prompt_cache_key(&self) -> Option<&str> {
        self.prompt_cache_key.as_deref()
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
    let started = Instant::now();
    let max_attempts = policy.max_attempts.max(1);

    for attempt_number in 1..=max_attempts {
        match send().await {
            Ok(response) => {
                if !is_retryable_status(response.status()) || attempt_number >= max_attempts {
                    return Ok(response);
                }

                let retry_number = attempt_number;
                let reason = format!("HTTP {}", response.status());
                let delay = retry_delay_from_response(policy, retry_number, &response);
                if !retry_fits_budget(policy, started, delay) {
                    return Ok(response);
                }
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
                if !is_retryable_error(&error) || attempt_number >= max_attempts {
                    return Err(error);
                }

                let retry_number = attempt_number;
                let delay = policy.jittered_backoff_for_retry(retry_number);
                if !retry_fits_budget(policy, started, delay) {
                    return Err(error);
                }
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

    unreachable!("max_attempts is normalized to at least one")
}

fn retry_fits_budget(policy: &HttpRetryPolicy, started: Instant, delay: Duration) -> bool {
    started
        .elapsed()
        .checked_add(delay)
        .is_some_and(|elapsed| elapsed <= policy.max_elapsed)
}

fn apply_jitter(delay: Duration, percent: u32) -> Duration {
    delay.mul_f64(f64::from(percent) / 100.0)
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
    parse_retry_after(response)
        .map(|delay| delay.min(policy.max_delay))
        .unwrap_or_else(|| policy.jittered_backoff_for_retry(retry_number))
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
#[expect(
    clippy::panic,
    clippy::panic_in_result_fn,
    reason = "fallible network fixture setup is combined with direct assertions"
)]
mod tests {
    use super::*;
    use color_eyre::{Result, eyre::eyre};
    use std::collections::VecDeque;
    use std::net::{SocketAddr, TcpListener as StdTcpListener};
    use std::sync::{Arc, Mutex};

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

    fn test_client_from_builder_or_skip(
        builder: reqwest::ClientBuilder,
    ) -> Option<reqwest::Client> {
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
}
