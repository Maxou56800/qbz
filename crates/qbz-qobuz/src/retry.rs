//! Transient-error retry helper for the streaming fetch path.
//!
//! A single transient network blip (5xx, timeout, connection reset, 429)
//! on the next track's `file/url` or a CMAF segment used to be terminal:
//! the play failed and the frontend auto-skipped the track, walking the
//! queue on a run of blips. Long Hi-Res tracks (Dream Theater / Yes) are
//! the worst case because their large downloads keep the link busy for
//! minutes, raising the odds of catching a blip. See issue #467.
//!
//! This module retries *transient* failures with exponential backoff while
//! letting *terminal* failures (a real 404 "gone forever", auth errors)
//! propagate immediately to the (now-bounded) skip path.

use std::future::Future;
use std::time::Duration;

/// Number of attempts: 1 initial try + 2 retries.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// A fetch error tagged with whether it is worth retrying. Used by the CMAF
/// CDN fetch path, whose underlying errors are otherwise stringly-typed.
#[derive(Debug)]
pub enum FetchError {
    /// Worth retrying: network/timeout/connect/body error, 5xx, or 429.
    Transient(String),
    /// Not worth retrying: 4xx (other than 429), or a definitive failure.
    Terminal(String),
}

impl FetchError {
    pub fn is_transient(&self) -> bool {
        matches!(self, FetchError::Transient(_))
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Transient(s) | FetchError::Terminal(s) => write!(f, "{}", s),
        }
    }
}

/// True for reqwest errors worth retrying: timeout, connect, request-build,
/// and body/decode errors are all transient transport-level problems.
pub fn reqwest_is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request() || e.is_body()
}

/// Classify a reqwest error into a `FetchError`. All reqwest transport errors
/// are treated as transient — a definitive "gone" answer comes back as a 404
/// *status*, not a transport error.
pub fn classify_reqwest(e: &reqwest::Error, context: &str) -> FetchError {
    // reqwest's Display embeds the full request URL, and CDN URLs carry a
    // signed token; this string is logged on every retry and bubbles up to
    // the player's warn lines. Describe the class, never the URL.
    FetchError::Transient(format!(
        "{}: {}",
        context,
        crate::net_diag::describe_reqwest_error(e)
    ))
}

/// Classify a non-success HTTP status into a `FetchError`. 5xx and 429 are
/// transient; everything else (404, 403, ...) is terminal.
pub fn classify_status(status: reqwest::StatusCode, context: &str) -> FetchError {
    let msg = format!("{}: HTTP {}", context, status);
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        FetchError::Transient(msg)
    } else {
        FetchError::Terminal(msg)
    }
}

/// Exponential backoff with jitter for the given 1-based attempt:
/// ~250 ms, ~500 ms, ~1 s, capped at 2 s, plus up to +25% jitter. Jitter is
/// derived from the wall clock to avoid pulling in a `rand` dependency.
fn backoff_delay(attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(3);
    let base_ms = 250u64.saturating_mul(1u64 << exp).min(2000);
    let jitter_span = base_ms / 4;
    let jitter = if jitter_span > 0 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        nanos % (jitter_span + 1)
    } else {
        0
    };
    Duration::from_millis(base_ms + jitter)
}

/// Run `op` (which takes the 1-based attempt number) and retry while it
/// returns a transient error, sleeping with exponential backoff between
/// attempts. Terminal errors and the final attempt return immediately.
pub async fn retry_transient<F, Fut, T, E>(
    max_attempts: u32,
    log_tag: &str,
    is_transient: impl Fn(&E) -> bool,
    mut op: F,
) -> std::result::Result<T, E>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = std::result::Result<T, E>>,
    E: std::fmt::Display,
{
    let mut attempt = 1;
    loop {
        match op(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= max_attempts || !is_transient(&err) {
                    return Err(err);
                }
                let delay = backoff_delay(attempt);
                log::warn!(
                    "[{}] transient error on attempt {}/{}: {} — retrying in {}ms",
                    log_tag,
                    attempt,
                    max_attempts,
                    err,
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn succeeds_first_try() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let r: Result<u32, FetchError> = retry_transient(3, "test", FetchError::is_transient, |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::Relaxed);
                Ok(42)
            }
        })
        .await;
        assert_eq!(r.unwrap(), 42);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let r: Result<u32, FetchError> = retry_transient(3, "test", FetchError::is_transient, |attempt| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::Relaxed);
                if attempt < 3 {
                    Err(FetchError::Transient("503".into()))
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(r.unwrap(), 7);
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn terminal_does_not_retry() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let r: Result<u32, FetchError> = retry_transient(3, "test", FetchError::is_transient, |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::Relaxed);
                Err(FetchError::Terminal("404".into()))
            }
        })
        .await;
        assert!(r.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let r: Result<u32, FetchError> = retry_transient(3, "test", FetchError::is_transient, |_| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::Relaxed);
                Err(FetchError::Transient("timeout".into()))
            }
        })
        .await;
        assert!(r.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }
}
