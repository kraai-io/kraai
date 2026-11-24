use std::time::{Duration, Instant};

use rand::RngExt;
use reqwest::{Response, ResponseBuilderExt, StatusCode};
use tracing::warn;

use crate::ProviderError;
use crate::rate_limit::RateLimit;
use crate::request_context::{ProviderRequestContext, ProviderRetryEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpRetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_delay: Duration,
    /// Attempts must enforce their own timeout; one started within this budget may complete
    /// after it expires. Time spent waiting out rate limits does not count.
    pub max_elapsed: Duration,
    /// Rate limits with a provider-given reset time, and the waits after them, don't count toward
    /// `max_attempts` or `max_elapsed`; this caps their total time instead.
    pub max_rate_limit_wait: Duration,
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

/// Shared across a provider's requests so a server's Retry-After pauses all of them.
#[derive(Debug, Default)]
pub struct ProviderCooldown {
    until: std::sync::Mutex<Option<tokio::time::Instant>>,
}

impl ProviderCooldown {
    fn remaining(&self) -> Duration {
        self.until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .map_or(Duration::ZERO, |until| {
                until.saturating_duration_since(tokio::time::Instant::now())
            })
    }

    fn defer(&self, delay: Duration) {
        let until = tokio::time::Instant::now() + delay;
        let mut current = self
            .until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.is_none_or(|current| current < until) {
            *current = Some(until);
        }
    }
}

pub const DEFAULT_HTTP_RETRY_POLICY: HttpRetryPolicy = HttpRetryPolicy {
    max_attempts: 10,
    initial_backoff: Duration::from_secs(1),
    max_delay: Duration::from_secs(15),
    max_elapsed: Duration::from_secs(300),
    max_rate_limit_wait: Duration::from_secs(30 * 60),
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
    let mut failed_attempts = 0;
    let mut retry_number = 0;
    let mut rate_limited = Duration::ZERO;
    let mut unpriced_prior_attempts = 0;

    loop {
        if let Some(cooldown) = request_context.cooldown() {
            let remaining = cooldown.remaining();
            if !fits_budget(rate_limited, remaining, policy.max_rate_limit_wait) {
                return Err(ProviderError::RateLimited {
                    message: format!("{operation} is paused by an earlier rate limit"),
                    retry_after: Some(remaining),
                }
                .into());
            }
            tokio::time::sleep(remaining).await;
            rate_limited += remaining;
        }
        if let Some(observer) = request_context.retry_observer() {
            observer.before_attempt(unpriced_prior_attempts).await?;
        }
        let attempt_started = Instant::now();
        match send().await {
            Ok(response) => {
                let status = response.status();
                let (response, rate_limit) = if status == StatusCode::TOO_MANY_REQUESTS {
                    let (response, rate_limit) = buffer_rate_limit(response).await?;
                    (response, Some(rate_limit))
                } else {
                    (response, None)
                };
                let retry_after = match rate_limit {
                    Some(RateLimit::Exhausted) => return Ok(response),
                    Some(RateLimit::ResetsIn(delay)) => Some(delay),
                    Some(RateLimit::Unknown) => None,
                    None => crate::rate_limit::retry_after(response.headers()),
                };
                if let (Some(cooldown), Some(retry_after)) =
                    (request_context.cooldown(), retry_after)
                    && matches!(
                        status,
                        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
                    )
                {
                    cooldown.defer(retry_after);
                }
                let is_rate_limit = matches!(rate_limit, Some(RateLimit::ResetsIn(_)));
                if is_rate_limit {
                    rate_limited += attempt_started.elapsed();
                } else {
                    failed_attempts += 1;
                }
                if !is_retryable_status(status) || failed_attempts >= max_attempts {
                    return Ok(response);
                }

                retry_number += 1;
                let reason = format!("HTTP {status}");
                let backoff = policy.jittered_backoff_for_retry(retry_number);
                let delay = retry_after.map_or(backoff, |retry_after| retry_after.max(backoff));
                let fits = if is_rate_limit {
                    fits_budget(rate_limited, delay, policy.max_rate_limit_wait)
                } else {
                    fits_budget(
                        started.elapsed().saturating_sub(rate_limited),
                        delay,
                        policy.max_elapsed,
                    )
                };
                if !fits {
                    return Ok(response);
                }
                if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
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
                if is_rate_limit {
                    rate_limited += delay;
                }
            }
            Err(error) => {
                failed_attempts += 1;
                if !is_retryable_error(&error) || failed_attempts >= max_attempts {
                    return Err(error.into());
                }

                retry_number += 1;
                let delay = policy.jittered_backoff_for_retry(retry_number);
                if !fits_budget(
                    started.elapsed().saturating_sub(rate_limited),
                    delay,
                    policy.max_elapsed,
                ) {
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
}

fn fits_budget(spent: Duration, delay: Duration, budget: Duration) -> bool {
    spent
        .checked_add(delay)
        .is_some_and(|total| total <= budget)
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

async fn buffer_rate_limit(response: Response) -> color_eyre::Result<(Response, RateLimit)> {
    let status = response.status();
    let version = response.version();
    let headers = response.headers().clone();
    let url = response.url().clone();
    let body = crate::read_error_body(response).await;
    let rate_limit = crate::rate_limit::classify(&headers, &body);
    let mut rebuilt = http::Response::builder()
        .status(status)
        .version(version)
        .url(url)
        .body(body)?;
    *rebuilt.headers_mut() = headers;
    Ok((Response::from(rebuilt), rate_limit))
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
mod tests;
