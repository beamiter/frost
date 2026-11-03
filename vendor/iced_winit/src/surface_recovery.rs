//! Pace failed native presentations, including input-triggered redraws.
use crate::core::time::Instant;
use std::time::Duration;

#[derive(Debug, Default)]
pub(crate) struct Recovery {
    failures: u32,
    retry_at: Option<Instant>,
}

impl Recovery {
    pub(crate) fn failed(&mut self, now: Instant) -> Instant {
        self.failures = self.failures.saturating_add(1);
        let delay =
            Duration::from_millis((100_u64 << self.failures.saturating_sub(1).min(5)).min(2_000));
        let deadline = now + delay;
        self.retry_at = Some(deadline);
        deadline
    }

    pub(crate) fn waiting_until(&self, now: Instant) -> Option<Instant> {
        self.retry_at.filter(|deadline| *deadline > now)
    }

    pub(crate) fn recovered(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_back_off_even_when_input_requests_more_frames() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        assert!(recovery.waiting_until(now).is_none());
        let deadline = recovery.failed(now);
        assert_eq!(deadline - now, Duration::from_millis(100));
        for _ in 0..100 {
            assert_eq!(recovery.waiting_until(now), Some(deadline));
        }
        assert!(recovery.waiting_until(deadline).is_none());
        let next = recovery.failed(deadline);
        assert_eq!(next - deadline, Duration::from_millis(200));
    }

    #[test]
    fn sustained_failure_is_bounded_and_success_resets_retry() {
        let mut now = Instant::now();
        let mut recovery = Recovery::default();
        for _ in 0..100 {
            let next = recovery.failed(now);
            assert!(next > now && next - now <= Duration::from_secs(2));
            now = next;
        }
        assert_eq!(recovery.failed(now) - now, Duration::from_secs(2));
        recovery.recovered();
        assert!(recovery.waiting_until(now).is_none());
        assert_eq!(recovery.failed(now) - now, Duration::from_millis(100));
    }
}
