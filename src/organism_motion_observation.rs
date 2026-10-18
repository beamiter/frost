//! Pure observation bookkeeping for the Frost Settings-portal adapter.
//! No DBus connection, timer, GUI, filesystem or environment access lives here.
//! Motion policy is imported from the single shared contract, not duplicated.

use jterm_core::organism_motion::SystemMotion;

/// Native decoding must supply only a validated u32 from the exact portal key.
/// Missing/error/wrong-type payloads are `None`, not a successful value zero.
pub const fn decode_portal_motion(value: Option<u32>) -> SystemMotion {
    match value {
        Some(1) => SystemMotion::Reduce,
        Some(_) => SystemMotion::NoPreference,
        None => SystemMotion::Unavailable,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderEpoch(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadTicket {
    epoch: ProviderEpoch,
    read: u64,
    signal_sequence: u64,
}

#[derive(Debug, Default)]
pub struct Observation {
    epoch: u64,
    active: bool,
    signal_sequence: u64,
    read_sequence: u64,
    pending: Option<ReadTicket>,
    value: SystemMotion,
}

impl Observation {
    pub fn value(&self) -> SystemMotion {
        self.value
    }

    /// Provider replacement also retires every old signal and in-flight read.
    /// Exhausted identity space fails closed; an epoch is never reused.
    pub fn begin_provider(&mut self) -> Option<ProviderEpoch> {
        self.active = false;
        self.pending = None;
        self.value = SystemMotion::Unavailable;
        self.epoch = self.epoch.checked_add(1)?;
        self.active = true;
        self.signal_sequence = 0;
        self.read_sequence = 0;
        Some(ProviderEpoch(self.epoch))
    }

    fn current(&self, epoch: ProviderEpoch) -> bool {
        self.active && epoch.0 == self.epoch
    }

    pub fn disconnect(&mut self, epoch: ProviderEpoch) -> bool {
        if !self.current(epoch) {
            return false;
        }
        self.active = false;
        self.pending = None;
        self.value = SystemMotion::Unavailable;
        true
    }

    pub fn begin_read(&mut self, epoch: ProviderEpoch) -> Option<ReadTicket> {
        if !self.current(epoch) {
            return None;
        }
        // Failure cannot leave an older request authoritative.
        self.pending = None;
        let Some(next_read) = self.read_sequence.checked_add(1) else {
            self.value = SystemMotion::Unavailable;
            return None;
        };
        self.read_sequence = next_read;
        let ticket = ReadTicket {
            epoch,
            read: self.read_sequence,
            signal_sequence: self.signal_sequence,
        };
        self.pending = Some(ticket);
        Some(ticket)
    }

    pub fn complete_read(&mut self, ticket: ReadTicket, value: SystemMotion) -> bool {
        if !self.current(ticket.epoch)
            || self.pending != Some(ticket)
            || ticket.signal_sequence != self.signal_sequence
        {
            return false;
        }
        self.pending = None;
        self.value = value;
        true
    }

    /// Sequence is assigned at validated signal receipt, before UI delivery.
    /// Zero, duplicate and reordered signals cannot supersede newer facts.
    pub fn signal(&mut self, epoch: ProviderEpoch, sequence: u64, value: SystemMotion) -> bool {
        if !self.current(epoch) || sequence <= self.signal_sequence {
            return false;
        }
        self.signal_sequence = sequence;
        self.pending = None;
        self.value = value;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::organism_motion::{resolve, Motion};

    #[test]
    fn protocol_zero_unknown_integer_and_unavailable_are_distinct() {
        assert_eq!(decode_portal_motion(Some(0)), SystemMotion::NoPreference);
        assert_eq!(decode_portal_motion(Some(1)), SystemMotion::Reduce);
        assert_eq!(decode_portal_motion(Some(2)), SystemMotion::NoPreference);
        assert_eq!(
            decode_portal_motion(Some(u32::MAX)),
            SystemMotion::NoPreference
        );
        assert_eq!(decode_portal_motion(None), SystemMotion::Unavailable);
    }

    #[test]
    fn initial_read_and_dynamic_change_use_the_shared_resolver() {
        let mut state = Observation::default();
        assert_eq!(resolve(None, state.value()).effective, Motion::Calm);
        let epoch = state.begin_provider().unwrap();
        let read = state.begin_read(epoch).unwrap();
        assert!(state.complete_read(read, SystemMotion::NoPreference));
        assert_eq!(resolve(None, state.value()).effective, Motion::Full);
        assert!(state.signal(epoch, 1, SystemMotion::Reduce));
        assert_eq!(resolve(None, state.value()).effective, Motion::Calm);
    }

    #[test]
    fn signal_retires_a_slow_initial_read() {
        let mut state = Observation::default();
        let epoch = state.begin_provider().unwrap();
        let read = state.begin_read(epoch).unwrap();
        assert!(state.signal(epoch, 1, SystemMotion::Reduce));
        assert!(!state.complete_read(read, SystemMotion::NoPreference));
        assert_eq!(state.value(), SystemMotion::Reduce);
    }

    #[test]
    fn latest_read_wins_and_completion_is_one_shot() {
        let mut state = Observation::default();
        let epoch = state.begin_provider().unwrap();
        let old = state.begin_read(epoch).unwrap();
        let new = state.begin_read(epoch).unwrap();
        assert!(!state.complete_read(old, SystemMotion::NoPreference));
        assert!(state.complete_read(new, SystemMotion::Reduce));
        assert!(!state.complete_read(new, SystemMotion::NoPreference));
        assert_eq!(state.value(), SystemMotion::Reduce);
    }

    #[test]
    fn duplicate_and_reordered_signals_cannot_undo_a_reduction() {
        let mut state = Observation::default();
        let epoch = state.begin_provider().unwrap();
        assert!(!state.signal(epoch, 0, SystemMotion::NoPreference));
        assert!(state.signal(epoch, 2, SystemMotion::Reduce));
        assert!(!state.signal(epoch, 1, SystemMotion::NoPreference));
        assert!(!state.signal(epoch, 2, SystemMotion::NoPreference));
        assert_eq!(state.value(), SystemMotion::Reduce);
    }

    #[test]
    fn disconnect_retires_read_and_signals_and_clears_full() {
        let mut state = Observation::default();
        let epoch = state.begin_provider().unwrap();
        state.signal(epoch, 1, SystemMotion::NoPreference);
        let read = state.begin_read(epoch).unwrap();
        assert!(state.disconnect(epoch));
        assert!(!state.complete_read(read, SystemMotion::NoPreference));
        assert!(!state.signal(epoch, 2, SystemMotion::NoPreference));
        assert!(state.begin_read(epoch).is_none());
        assert_eq!(resolve(None, state.value()).effective, Motion::Calm);
    }

    #[test]
    fn old_provider_events_cannot_disconnect_or_overwrite_new_owner() {
        let mut state = Observation::default();
        let old = state.begin_provider().unwrap();
        let read = state.begin_read(old).unwrap();
        let new = state.begin_provider().unwrap();
        assert!(state.signal(new, 1, SystemMotion::Reduce));
        assert!(!state.disconnect(old));
        assert!(!state.signal(old, u64::MAX, SystemMotion::NoPreference));
        assert!(!state.complete_read(read, SystemMotion::NoPreference));
        assert_eq!(state.value(), SystemMotion::Reduce);
    }

    #[test]
    fn failed_current_read_is_unavailable_not_no_preference() {
        let mut state = Observation::default();
        let epoch = state.begin_provider().unwrap();
        state.signal(epoch, 1, SystemMotion::NoPreference);
        let read = state.begin_read(epoch).unwrap();
        assert!(state.complete_read(read, decode_portal_motion(None)));
        assert_eq!(resolve(None, state.value()).effective, Motion::Calm);
    }

    #[test]
    fn explicit_configuration_survives_observation_and_disconnect() {
        for explicit in [Motion::Full, Motion::Calm, Motion::Static] {
            let mut state = Observation::default();
            let epoch = state.begin_provider().unwrap();
            for (seq, value) in [(1, SystemMotion::NoPreference), (2, SystemMotion::Reduce)] {
                assert!(state.signal(epoch, seq, value));
                assert_eq!(resolve(Some(explicit), state.value()).effective, explicit);
            }
            state.disconnect(epoch);
            assert_eq!(resolve(Some(explicit), state.value()).effective, explicit);
        }
    }

    #[test]
    fn exhausted_provider_identity_fails_closed_without_reuse() {
        let mut state = Observation {
            epoch: u64::MAX,
            value: SystemMotion::NoPreference,
            ..Default::default()
        };
        assert!(state.begin_provider().is_none());
        assert!(state.begin_provider().is_none());
        assert_eq!(state.value(), SystemMotion::Unavailable);
    }

    #[test]
    fn exhausted_read_identity_retires_the_previous_request() {
        let mut state = Observation::default();
        let epoch = state.begin_provider().unwrap();
        assert!(state.signal(epoch, 1, SystemMotion::NoPreference));
        assert_eq!(resolve(None, state.value()).effective, Motion::Full);
        let old = state.begin_read(epoch).unwrap();
        state.read_sequence = u64::MAX;
        assert!(state.begin_read(epoch).is_none());
        assert!(!state.complete_read(old, SystemMotion::NoPreference));
        assert_eq!(state.value(), SystemMotion::Unavailable);
    }

    #[test]
    fn a_fresh_read_after_a_signal_can_complete() {
        let mut state = Observation::default();
        let epoch = state.begin_provider().unwrap();
        assert!(state.signal(epoch, 1, SystemMotion::Reduce));
        let read = state.begin_read(epoch).unwrap();
        assert!(state.complete_read(read, SystemMotion::NoPreference));
        assert_eq!(state.value(), SystemMotion::NoPreference);
    }

    #[test]
    fn ignored_signals_do_not_retire_a_current_read() {
        let mut state = Observation::default();
        let old = state.begin_provider().unwrap();
        let current = state.begin_provider().unwrap();
        assert!(state.signal(current, 2, SystemMotion::Reduce));
        let read = state.begin_read(current).unwrap();
        assert!(!state.signal(current, 2, SystemMotion::NoPreference));
        assert!(!state.signal(current, 1, SystemMotion::NoPreference));
        assert!(!state.signal(old, 3, SystemMotion::NoPreference));
        assert!(state.complete_read(read, SystemMotion::NoPreference));
    }
}
