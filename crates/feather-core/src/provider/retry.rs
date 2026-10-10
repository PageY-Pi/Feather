//! Provider retry policy shared by the agent loop and tests.
//!
//! t019: one initial attempt plus three automatic retries whose deadlines are
//! anchored to the first failure at +5, +30 and +60 seconds (not cumulative
//! 5/30/60-second sleeps). Time already spent on failed requests counts against
//! each deadline, so an overdue retry runs at once. Quota exhaustion is a
//! billing/capacity terminal condition and is intentionally not treated like
//! ordinary rate limits.

use super::ProviderError;
use std::time::Duration;
use tokio::time::Instant;

/// Offsets, measured once from the first error, at which each retry may run.
pub const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
pub const MAX_ATTEMPTS: usize = 1 + RETRY_DELAYS.len();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPlan {
    /// Zero-based failed attempt index.
    pub failed_attempt: usize,
    /// One-based attempt number that will run after the delay.
    pub next_attempt: usize,
    /// Offset from the *first* error at which this retry may run.
    pub deadline: Duration,
}

pub fn plan(error: &ProviderError, failed_attempt: usize) -> Option<RetryPlan> {
    if failed_attempt >= RETRY_DELAYS.len() || !is_retryable(error) {
        return None;
    }
    Some(RetryPlan {
        failed_attempt,
        next_attempt: failed_attempt + 2,
        deadline: RETRY_DELAYS[failed_attempt],
    })
}

/// Wait still owed before `plan`'s retry may run.
///
/// `first_error` is the instant the initial failure occurred and `now` is the
/// current instant. Because every deadline is anchored to `first_error`, time
/// already spent (including the failed request itself) is subtracted; once the
/// deadline has passed this returns [`Duration::ZERO`] so the retry runs
/// immediately instead of waiting again.
pub fn delay_until(plan: &RetryPlan, first_error: Instant, now: Instant) -> Duration {
    (first_error + plan.deadline).saturating_duration_since(now)
}

pub fn is_retryable(error: &ProviderError) -> bool {
    !matches!(error, ProviderError::Aborted) && !is_quota_exhausted(error)
}

pub fn is_quota_exhausted(error: &ProviderError) -> bool {
    match error {
        ProviderError::Api { body, .. }
        | ProviderError::Http(body)
        | ProviderError::Stream(body)
        | ProviderError::Auth(body) => looks_like_quota_exhausted(body),
        _ => false,
    }
}

fn looks_like_quota_exhausted(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let has_quota_code = [
        "insufficient_quota",
        "quota_exceeded",
        "usage_limit",
        "monthly_quota",
        "quota_auth",
        "billing_hard_limit",
    ]
    .iter()
    .any(|needle| compact.contains(needle));
    has_quota_code
        || s.contains("quota exhausted")
        || s.contains("exceeded your current quota")
        || s.contains("monthly quota")
        || s.contains("billing hard limit")
        || s.contains("credit balance")
        || s.contains("out of credits")
}
pub async fn sleep_or_abort(
    delay: Duration,
    abort: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), ProviderError> {
    if *abort.borrow() {
        return Err(ProviderError::Aborted);
    }
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return Ok(()),
            changed = abort.changed() => {
                if changed.is_err() || *abort.borrow() {
                    return Err(ProviderError::Aborted);
                }
                // Ignore false notifications; the backoff must not end early.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_rate_limits_but_not_quota() {
        let rate = ProviderError::Api { status: 429, body: "rate limit".into() };
        assert!(plan(&rate, 0).is_some());

        for body in [
            "insufficient_quota",
            r#"{"error":{"code":"usage_limit","message":"monthly quota reached"}}"#,
            r#"{"error":{"type":"quota_auth"}}"#,
            "exceeded your current quota",
        ] {
            let quota = ProviderError::Api { status: 429, body: body.into() };
            assert!(plan(&quota, 0).is_none(), "quota body retried: {body}");
        }

        let nonquota = ProviderError::Api { status: 429, body: "rate limit: try later".into() };
        assert_eq!(plan(&nonquota, 0).unwrap().deadline, Duration::from_secs(5));
        assert_eq!(plan(&nonquota, 1).unwrap().deadline, Duration::from_secs(30));
        assert_eq!(plan(&nonquota, 2).unwrap().deadline, Duration::from_secs(60));
        assert!(plan(&nonquota, 3).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn abort_interrupts_backoff() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            sleep_or_abort(Duration::from_secs(30), &mut rx).await
        });
        tx.send(true).unwrap();
        assert!(matches!(task.await.unwrap(), Err(ProviderError::Aborted)));
    }

    #[tokio::test(start_paused = true)]
    async fn false_abort_notification_does_not_end_backoff() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            sleep_or_abort(Duration::from_secs(30), &mut rx).await
        });
        tx.send_replace(false);
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "false watch update ended retry backoff early");
        tokio::time::advance(Duration::from_secs(30)).await;
        assert!(task.await.unwrap().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn deadlines_are_anchored_to_the_first_error_not_sequential() {
        let err = ProviderError::Http("transient".into());
        let t0 = Instant::now();
        let first = plan(&err, 0).unwrap();
        let second = plan(&err, 1).unwrap();
        let third = plan(&err, 2).unwrap();

        // From the initial error each retry is due at +5, +30 and +60 seconds.
        assert_eq!(delay_until(&first, t0, t0), Duration::from_secs(5));
        assert_eq!(delay_until(&second, t0, t0), Duration::from_secs(30));
        assert_eq!(delay_until(&third, t0, t0), Duration::from_secs(60));

        // Once the first wait elapses only the remaining 25s is owed for the
        // second retry (30 - 5), not a fresh 30s sleep.
        let after_first = t0 + Duration::from_secs(5);
        assert_eq!(delay_until(&second, t0, after_first), Duration::from_secs(25));
        assert_eq!(delay_until(&third, t0, after_first), Duration::from_secs(55));
    }

    #[tokio::test(start_paused = true)]
    async fn elapsed_request_time_is_subtracted_and_past_deadlines_run_now() {
        let err = ProviderError::Http("transient".into());
        let t0 = Instant::now();
        let first = plan(&err, 0).unwrap();
        let third = plan(&err, 2).unwrap();

        // A slow request that already burned past +5s owes no further wait.
        let elapsed = t0 + Duration::from_secs(8);
        assert_eq!(delay_until(&first, t0, elapsed), Duration::ZERO);
        // Even far past +60s the wait saturates at zero instead of going negative.
        let way_late = t0 + Duration::from_secs(120);
        assert_eq!(delay_until(&first, t0, way_late), Duration::ZERO);
        assert_eq!(delay_until(&third, t0, way_late), Duration::ZERO);

        // A zero wait completes without advancing the clock.
        let before = Instant::now();
        let (_tx, mut rx) = tokio::sync::watch::channel(false);
        assert!(sleep_or_abort(Duration::ZERO, &mut rx).await.is_ok());
        assert_eq!(Instant::now(), before, "a zero wait must not advance the clock");
    }

    #[tokio::test(start_paused = true)]
    async fn abort_before_backoff_is_immediate() {
        let before = Instant::now();
        let (_tx, mut rx) = tokio::sync::watch::channel(true);
        assert!(matches!(
            sleep_or_abort(Duration::from_secs(60), &mut rx).await,
            Err(ProviderError::Aborted)
        ));
        // An already-set abort returns at once: the clock did not move.
        assert_eq!(Instant::now(), before);
    }
}
