//! Content-free, volatile observation of the one visible running command.
use std::time::Duration;

use jterm_core::organism::WatchRhythm;

#[derive(Default)]
pub(crate) struct WatchObservation {
    owner: Option<usize>,
    started: Option<Duration>,
    skip_first_output: bool,
    outputs: [Option<Duration>; 3],
    resumed_until: Option<Duration>,
}

impl WatchObservation {
    const BUSY_WINDOW: Duration = Duration::from_millis(1200);
    const WAITING_AFTER: Duration = Duration::from_secs(3);
    const RESUMED_HOLD: Duration = Duration::from_millis(900);

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn reset_owner(&mut self, id: usize) {
        if self.owner == Some(id) {
            self.reset();
        }
    }

    /// Returns true only for an already established observation.
    pub fn observe(&mut self, owner: Option<usize>, running: bool, now: Duration) -> bool {
        let owner = owner.filter(|_| running);
        if owner.is_none() || owner != self.owner {
            self.reset();
            self.owner = owner;
            self.started = owner.map(|_| now);
            self.skip_first_output = owner.is_some();
            false
        } else {
            true
        }
    }

    pub fn output(&mut self, id: usize, now: Duration, admitted: bool) {
        if self.owner != Some(id) {
            return;
        }
        if std::mem::take(&mut self.skip_first_output) {
            // The first observed batch is neutral, even after a long silence.
            // It starts a fresh quiet baseline without counting an event.
            self.started = Some(now);
            self.outputs = [None; 3];
            self.resumed_until = None;
            return;
        }
        if !admitted {
            return;
        }
        if self.outputs[2]
            .or(self.started)
            .is_some_and(|last| now.saturating_sub(last) >= Self::WAITING_AFTER)
        {
            self.resumed_until = Some(now.saturating_add(Self::RESUMED_HOLD));
        }
        self.outputs.rotate_left(1);
        self.outputs[2] = Some(now);
    }

    pub fn rhythm(&self, id: usize, now: Duration) -> WatchRhythm {
        if self.owner != Some(id) {
            return WatchRhythm::Steady;
        }
        if self.resumed_until.is_some_and(|until| now < until) {
            WatchRhythm::Resumed
        } else if self.outputs[2]
            .or(self.started)
            .is_some_and(|last| now.saturating_sub(last) >= Self::WAITING_AFTER)
        {
            WatchRhythm::Waiting
        } else if self.outputs[0]
            .is_some_and(|oldest| now.saturating_sub(oldest) <= Self::BUSY_WINDOW)
        {
            WatchRhythm::Busy
        } else {
            WatchRhythm::Steady
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn three_events_in_window_are_busy_then_quiet_becomes_waiting() {
        let mut watch = WatchObservation::default();
        assert!(!watch.observe(Some(1), true, ms(0)));
        watch.output(1, ms(0), true);
        watch.output(1, ms(100), true);
        watch.output(1, ms(600), true);
        assert_eq!(watch.rhythm(1, ms(600)), WatchRhythm::Steady);
        watch.output(1, ms(1300), true);
        assert_eq!(watch.rhythm(1, ms(1300)), WatchRhythm::Busy);
        assert_eq!(watch.rhythm(1, ms(1301)), WatchRhythm::Steady);
        assert_eq!(watch.rhythm(1, ms(4299)), WatchRhythm::Steady);
        assert_eq!(watch.rhythm(1, ms(4300)), WatchRhythm::Waiting);
    }

    #[test]
    fn silence_from_observation_and_resume_have_exact_boundaries() {
        let mut watch = WatchObservation::default();
        watch.observe(Some(1), true, ms(0));
        watch.output(1, ms(0), true);
        assert_eq!(watch.rhythm(1, ms(2999)), WatchRhythm::Steady);
        assert_eq!(watch.rhythm(1, ms(3000)), WatchRhythm::Waiting);
        watch.output(1, ms(3000), true);
        watch.output(1, ms(3100), true);
        watch.output(1, ms(3200), true);
        assert_eq!(watch.rhythm(1, ms(3899)), WatchRhythm::Resumed);
        assert_eq!(watch.rhythm(1, ms(3900)), WatchRhythm::Busy);
        assert_eq!(watch.rhythm(1, ms(4201)), WatchRhythm::Steady);
        assert_eq!(watch.rhythm(1, ms(6200)), WatchRhythm::Waiting);
    }

    #[test]
    fn delayed_first_output_establishes_a_neutral_quiet_baseline() {
        let mut watch = WatchObservation::default();
        watch.observe(Some(1), true, ms(0));
        assert_eq!(watch.rhythm(1, ms(4999)), WatchRhythm::Waiting);
        watch.output(1, ms(5000), true);
        assert_eq!(watch.rhythm(1, ms(5000)), WatchRhythm::Steady);
        assert_eq!(watch.rhythm(1, ms(7999)), WatchRhythm::Steady);
        assert_eq!(watch.rhythm(1, ms(8000)), WatchRhythm::Waiting);
    }

    #[test]
    fn repeated_sync_cannot_admit_the_first_restored_output_batch() {
        let mut watch = WatchObservation::default();
        watch.observe(Some(1), true, ms(0));
        watch.observe(None, true, ms(3000));
        // The host syncs at update entry, then again after processing output.
        watch.observe(Some(1), true, ms(9000));
        assert!(watch.observe(Some(1), true, ms(9000)));
        watch.output(1, ms(9000), true);
        watch.output(1, ms(9100), true);
        watch.output(1, ms(9200), true);
        assert_eq!(watch.rhythm(1, ms(9200)), WatchRhythm::Steady);
        watch.output(1, ms(9300), true);
        assert_eq!(watch.rhythm(1, ms(9300)), WatchRhythm::Busy);
        watch.reset_owner(1);
        watch.observe(Some(1), true, ms(9400));
        watch.output(1, ms(9400), false);
        watch.output(1, ms(9500), true);
        watch.output(1, ms(9600), true);
        assert_eq!(watch.rhythm(1, ms(9600)), WatchRhythm::Steady);
    }

    #[test]
    fn hidden_switched_and_completed_observations_never_catch_up() {
        for next in [None, Some(2)] {
            let mut watch = WatchObservation::default();
            watch.observe(Some(1), true, ms(0));
            watch.output(1, ms(3000), true);
            watch.observe(next, true, ms(3100));
            watch.output(1, ms(6000), true);
            assert!(!watch.observe(Some(1), true, ms(9000)));
            assert_eq!(watch.rhythm(1, ms(9000)), WatchRhythm::Steady);
            assert_eq!(watch.rhythm(1, ms(11999)), WatchRhythm::Steady);
        }
        let mut watch = WatchObservation::default();
        watch.observe(Some(1), true, ms(0));
        watch.observe(Some(1), false, ms(4000));
        watch.output(1, ms(5000), true);
        watch.observe(Some(1), true, ms(9000));
        assert_eq!(watch.rhythm(1, ms(9000)), WatchRhythm::Steady);
        watch.reset_owner(2);
        assert_eq!(watch.rhythm(1, ms(12000)), WatchRhythm::Waiting);
        watch.reset_owner(1);
        assert_eq!(watch.rhythm(1, ms(12000)), WatchRhythm::Steady);
    }
}
