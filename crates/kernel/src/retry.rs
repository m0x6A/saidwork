//! Retry with exponential backoff for transient failures.

use std::future::Future;
use std::time::Duration;

/// How many attempts to make and how long to wait between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first one; `1` disables retrying.
    pub attempts: u32,
    /// Delay before the second attempt; doubles each time up to `max_delay`.
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// Three attempts, 500 ms then 1 s.
    #[must_use]
    pub const fn default_http() -> Self {
        Self {
            attempts: 3,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(5),
        }
    }

    /// Single attempt; useful in tests.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            attempts: 1,
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    #[must_use]
    pub fn delay_for(&self, failed_attempts: u32) -> Duration {
        let factor = 2u32.saturating_pow(failed_attempts.saturating_sub(1));
        self.initial_delay
            .saturating_mul(factor)
            .min(self.max_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::default_http()
    }
}

/// Run `op` until it succeeds, the error is not transient, or attempts are exhausted.
pub async fn retry<T, E, Fut>(
    policy: RetryPolicy,
    is_transient: impl Fn(&E) -> bool,
    mut op: impl FnMut(u32) -> Fut,
) -> Result<T, E>
where
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let attempts = policy.attempts.max(1);
    let mut attempt = 1;
    loop {
        match op(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) if attempt < attempts && is_transient(&err) => {
                let delay = policy.delay_for(attempt);
                tracing::warn!(attempt, %err, ?delay, "transient failure, retrying");
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn fast(attempts: u32) -> RetryPolicy {
        RetryPolicy {
            attempts,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        }
    }

    #[tokio::test]
    async fn retries_transient_errors_until_success() {
        let calls = AtomicU32::new(0);
        let out: Result<u32, String> = retry(
            fast(5),
            |_| true,
            |n| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move { if n < 3 { Err("boom".to_owned()) } else { Ok(n) } }
            },
        )
        .await;
        assert_eq!(out.unwrap(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn gives_up_after_attempts() {
        let calls = AtomicU32::new(0);
        let out: Result<(), String> = retry(
            fast(3),
            |_| true,
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err("boom".to_owned()) }
            },
        )
        .await;
        assert_eq!(out.unwrap_err(), "boom");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_permanent_errors() {
        let calls = AtomicU32::new(0);
        let out: Result<(), String> = retry(
            fast(3),
            |e| e != "fatal",
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err("fatal".to_owned()) }
            },
        )
        .await;
        assert!(out.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy {
            attempts: 5,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(350),
        };
        assert_eq!(p.delay_for(1), Duration::from_millis(100));
        assert_eq!(p.delay_for(2), Duration::from_millis(200));
        assert_eq!(p.delay_for(3), Duration::from_millis(350));
    }
}
