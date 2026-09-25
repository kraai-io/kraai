use std::future::Future;
use std::time::{Duration, Instant, SystemTime};

use rand::RngExt;
use reqwest::header::RETRY_AFTER;
use reqwest::{Response, StatusCode};
use tracing::warn;

use crate::request_context::{ProviderRequestContext, ProviderRetryEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpRetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_delay: Duration,
    /// Maximum elapsed time before scheduling another retry delay.
    ///
    /// Individual request attempts must enforce their own timeout; an attempt
    /// started within this budget may complete after it expires.
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

pub async fn send_with_retry<F, Fut>(
    operation: &'static str,
    policy: &HttpRetryPolicy,
    request_context: &ProviderRequestContext,
    mut send: F,
) -> color_eyre::Result<Response>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<Response>>,
{
    let started = Instant::now();
    let max_attempts = policy
        .max_attempts
        .max(1)
        .min(request_context.max_attempts().unwrap_or(u32::MAX));
    let mut unpriced_prior_attempts = 0;

    for attempt_number in 1..=max_attempts {
        if let Some(observer) = request_context.retry_observer() {
            observer.before_attempt(unpriced_prior_attempts).await?;
        }
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
                if response.status().is_server_error()
                    || response.status() == StatusCode::REQUEST_TIMEOUT
                {
                    unpriced_prior_attempts += 1;
                }
                notify_retry(request_context, operation, retry_number, delay, &reason);
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
                    return Err(error.into());
                }

                let retry_number = attempt_number;
                let delay = policy.jittered_backoff_for_retry(retry_number);
                if !retry_fits_budget(policy, started, delay) {
                    return Err(error.into());
                }
                if !error.is_connect() {
                    unpriced_prior_attempts += 1;
                }
                let reason = error.to_string();
                notify_retry(request_context, operation, retry_number, delay, &reason);
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
    reason: &str,
) {
    if let Some(observer) = request_context.retry_observer() {
        observer.on_retry_scheduled(&ProviderRetryEvent {
            operation,
            retry_number,
            delay,
            reason: reason.to_string(),
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
mod tests;
