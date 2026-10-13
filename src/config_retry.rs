//! Automatic config-save retry policy. No timers, I/O, or config snapshots.
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct ConfigSaveRetry {
    failures: u8,
    next_attempt: Option<Instant>,
    last_error: Option<String>,
}

impl ConfigSaveRetry {
    pub(crate) fn ready(&self, now: Instant) -> bool {
        self.next_attempt.is_none_or(|deadline| now >= deadline)
    }

    /// Sample `now` after the save returns: a slow lock failure must not
    /// consume its own backoff. Report the first or a changed error only.
    pub(crate) fn failed(&mut self, now: Instant, error: String) -> bool {
        let delay =
            Duration::from_millis(1500u64 << self.failures.min(5)).min(Duration::from_secs(30));
        self.failures = self.failures.saturating_add(1);
        self.next_attempt = now.checked_add(delay);
        let report = self.last_error.as_ref() != Some(&error);
        self.last_error = Some(error);
        report
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_cap_and_deadline_boundary_is_inclusive() {
        let mut retry = ConfigSaveRetry::default();
        let mut now = Instant::now();
        assert!(retry.ready(now));
        for millis in [1500, 3000, 6000, 12000, 24000, 30000, 30000] {
            retry.failed(now, "same".into());
            let deadline = now + Duration::from_millis(millis);
            assert!(!retry.ready(deadline - Duration::from_nanos(1)));
            assert!(retry.ready(deadline));
            now = deadline;
        }
    }

    #[test]
    fn slow_failure_backoff_begins_after_completion() {
        let start = Instant::now();
        let finished = start + Duration::from_secs(4);
        let mut retry = ConfigSaveRetry::default();
        retry.failed(finished, "locked".into());
        assert!(!retry.ready(finished));
        assert!(!retry.ready(finished + Duration::from_millis(1499)));
        assert!(retry.ready(finished + Duration::from_millis(1500)));
    }

    #[test]
    fn repeated_errors_are_quiet_but_changed_errors_are_reported() {
        let now = Instant::now();
        let mut retry = ConfigSaveRetry::default();
        assert!(retry.failed(now, "Locked: one".into()));
        assert!(!retry.failed(now, "Locked: one".into()));
        assert!(retry.failed(now, "Io: one".into()));
        assert!(retry.failed(now, "Io: two".into()));
        assert!(retry.failed(now, "Conflict: two".into()));
        assert!(!retry.failed(now, "Conflict: two".into()));
    }

    #[test]
    fn reset_restores_immediate_attempt_and_first_error_reporting() {
        let now = Instant::now();
        let mut retry = ConfigSaveRetry::default();
        retry.failed(now, "same".into());
        assert!(!retry.ready(now));
        retry.reset();
        assert!(retry.ready(now));
        assert!(retry.failed(now, "same".into()));
        assert!(retry.ready(now + Duration::from_millis(1500)));
    }

    #[test]
    fn long_failure_chain_saturates_without_overflow() {
        let mut now = Instant::now();
        let mut retry = ConfigSaveRetry::default();
        for _ in 0..6 {
            retry.failed(now, "same".into());
            now += Duration::from_secs(30);
        }
        for _ in 0..1000 {
            retry.failed(now, "same".into());
            assert!(!retry.ready(now + Duration::from_secs(29)));
            now += Duration::from_secs(30);
            assert!(retry.ready(now));
        }
        assert_eq!(retry.failures, u8::MAX);
    }

    #[test]
    fn forty_virtual_config_ticks_do_not_retry_every_tick() {
        let start = Instant::now();
        let mut retry = ConfigSaveRetry::default();
        let mut attempts = 0;
        for tick in 0..40 {
            let now = start + Duration::from_millis(tick * 1500);
            if retry.ready(now) {
                attempts += 1;
                retry.failed(now, "same".into());
            }
        }
        assert_eq!(attempts, 6);
    }
}
