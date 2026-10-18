//! Volatile host observation identity. All motion policy lives in jterm_core.
use crate::organism_system_motion::{Event, Source};
use jterm_core::organism_motion::{self, Motion, Resolution, SystemMotion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub previous: Motion,
    pub effective: Motion,
}

#[derive(Debug)]
pub struct MotionControl {
    configured: Option<Motion>,
    observed: SystemMotion,
    source: Source,
    epoch: u64,
    sequence: u64,
    active: bool,
    exhausted: bool,
}

impl Default for MotionControl {
    fn default() -> Self {
        Self {
            configured: None,
            observed: SystemMotion::Unavailable,
            source: Source::Initializing,
            epoch: 0,
            sequence: 0,
            active: false,
            exhausted: false,
        }
    }
}

impl MotionControl {
    pub fn resolution(&self) -> Resolution {
        organism_motion::resolve(self.configured, self.observed)
    }

    pub fn source(&self) -> Source {
        self.source
    }

    pub fn subscription_epoch(&self) -> Option<u64> {
        self.active.then_some(self.epoch)
    }

    /// A configured choice is supplied by the existing config owner. System
    /// events cannot call this method or write that persisted configuration.
    pub fn reconcile(
        &mut self,
        configured: Option<Motion>,
        relevant: bool,
        foreground_resume: bool,
    ) -> Option<Transition> {
        let previous = self.resolution().effective;
        let wanted = configured.is_none() && relevant && !self.exhausted;
        if self.configured != configured || self.active != wanted || (foreground_resume && wanted) {
            self.configured = configured;
            self.sequence = 0;
            self.observed = SystemMotion::Unavailable;
            if let Some(epoch) = self.epoch.checked_add(1) {
                self.epoch = epoch;
                self.active = wanted;
                self.source = if wanted {
                    Source::Initializing
                } else {
                    Source::Unavailable
                };
            } else {
                self.exhausted = true;
                self.active = false;
                self.source = Source::IdentityExhausted;
            }
        }
        self.transition(previous)
    }

    pub fn observe(&mut self, event: Event) -> Option<Transition> {
        if !self.active || event.subscription_epoch != self.epoch || event.sequence <= self.sequence
        {
            return None;
        }
        let previous = self.resolution().effective;
        self.sequence = event.sequence;
        self.observed = event.observed;
        self.source = event.source;
        self.transition(previous)
    }

    fn transition(&self, previous: Motion) -> Option<Transition> {
        let effective = self.resolution().effective;
        (previous != effective).then_some(Transition {
            previous,
            effective,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(control: &MotionControl, sequence: u64, observed: SystemMotion) -> Event {
        Event {
            subscription_epoch: control.epoch,
            sequence,
            observed,
            source: Source::Portal,
        }
    }

    #[test]
    fn fallback_to_reduce_changes_source_without_resetting_effective_motion() {
        let mut control = MotionControl::default();
        assert_eq!(control.reconcile(None, true, false), None);
        let event = event(&control, 1, SystemMotion::Reduce);
        assert_eq!(control.observe(event), None);
        assert_eq!(control.source(), Source::Portal);
        assert_eq!(control.resolution().effective, Motion::Calm);
    }

    #[test]
    fn explicit_choice_retires_observation_and_reentry_starts_unknown() {
        let mut control = MotionControl::default();
        control.reconcile(None, true, false);
        let full = event(&control, 1, SystemMotion::NoPreference);
        assert_eq!(control.observe(full).unwrap().effective, Motion::Full);
        control.reconcile(Some(Motion::Static), true, false);
        assert_eq!(control.subscription_epoch(), None);
        assert_eq!(control.observe(full), None);
        assert_eq!(control.resolution().effective, Motion::Static);
        control.reconcile(None, true, false);
        assert_eq!(control.resolution().effective, Motion::Calm);
        assert_eq!(control.observed, SystemMotion::Unavailable);
    }

    #[test]
    fn stale_duplicate_and_reordered_ui_events_cannot_restore_full() {
        let mut control = MotionControl::default();
        control.reconcile(None, true, false);
        let stale = event(&control, 1, SystemMotion::NoPreference);
        let reduced = event(&control, 2, SystemMotion::Reduce);
        control.observe(reduced);
        assert_eq!(control.observe(stale), None);
        assert_eq!(control.observe(reduced), None);
        control.reconcile(None, true, true);
        assert_eq!(control.observe(stale), None);
        assert_eq!(control.resolution().effective, Motion::Calm);
    }

    #[test]
    fn irrelevant_subscription_closes_without_affecting_explicit_full() {
        let mut control = MotionControl::default();
        control.reconcile(None, true, false);
        let full = event(&control, 1, SystemMotion::NoPreference);
        control.observe(full);
        assert_eq!(
            control.reconcile(None, false, false).unwrap().effective,
            Motion::Calm
        );
        assert_eq!(control.subscription_epoch(), None);
        control.reconcile(Some(Motion::Full), false, false);
        assert_eq!(control.resolution().effective, Motion::Full);
        assert_eq!(control.subscription_epoch(), None);
    }

    #[test]
    fn unchanged_config_or_relevance_does_not_restart_the_subscription() {
        let mut control = MotionControl::default();
        control.reconcile(None, true, false);
        let full = event(&control, 1, SystemMotion::NoPreference);
        control.observe(full);
        let epoch = control.subscription_epoch();
        assert_eq!(control.reconcile(None, true, false), None);
        assert_eq!(control.subscription_epoch(), epoch);
        assert_eq!(control.resolution().effective, Motion::Full);
    }

    #[test]
    fn identity_exhaustion_never_reuses_a_subscription_or_overrides_explicit_choice() {
        let mut control = MotionControl {
            epoch: u64::MAX,
            ..MotionControl::default()
        };
        control.reconcile(None, true, false);
        assert_eq!(control.subscription_epoch(), None);
        assert_eq!(control.source(), Source::IdentityExhausted);
        control.reconcile(Some(Motion::Full), true, true);
        assert_eq!(control.resolution().effective, Motion::Full);
        control.reconcile(None, true, true);
        assert_eq!(control.resolution().effective, Motion::Calm);
        assert_eq!(control.subscription_epoch(), None);
    }
}
