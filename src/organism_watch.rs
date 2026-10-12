//! Native provenance adapter. Cadence and deadlines belong to the shared reducer.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jterm_core::organism::WatchRhythm;
use jterm_core::organism_watch::{
    WatchEvent, WatchObservation as SharedWatch, WatchOutputDisposition,
};

use crate::terminal::WatchBatch;

/// Shared only by one Session and its reader incarnations. Zero disables Watch.
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceAuthority(Arc<AtomicU64>);

#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceStamp {
    pub epoch: u64,
    pub seq: u64,
    pub read_at: Instant,
}

#[derive(Debug)]
pub(crate) struct SourceWriter {
    authority: SourceAuthority,
    epoch: u64,
    next: Option<u64>,
}

impl SourceAuthority {
    pub fn epoch(&self) -> Option<u64> {
        let value = self.0.load(Ordering::Acquire);
        (value != 0).then_some(value)
    }

    pub fn start(&self) -> SourceWriter {
        static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);
        let mut candidate = NEXT_SOURCE.load(Ordering::Acquire);
        let epoch = loop {
            let Some(next) = candidate.checked_add(1) else {
                break 0;
            };
            match NEXT_SOURCE.compare_exchange_weak(
                candidate,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break candidate,
                Err(current) => candidate = current,
            }
        };
        // Publishing the incarnation is the linearization point. Numeric order
        // is irrelevant; uniqueness and matching this Session's authority matter.
        self.0.store(epoch, Ordering::Release);
        SourceWriter {
            authority: self.clone(),
            epoch,
            next: (epoch != 0).then_some(0),
        }
    }
}

impl SourceWriter {
    pub fn stamp(&mut self, read_at: Instant) -> Option<SourceStamp> {
        let seq = self.next?;
        self.next = seq.checked_add(1);
        if self.next.is_none() {
            // Fail closed only for Watch. The reader still sends every byte.
            let _ = self.authority.0.compare_exchange(
                self.epoch,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return None;
        }
        Some(SourceStamp {
            epoch: self.epoch,
            seq,
            read_at,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct SourceReceiver {
    epoch: Option<u64>,
    seq: Option<u64>,
}

impl SourceReceiver {
    /// Validate before the host's generic update-entry observation/clock sync.
    pub fn accept(&mut self, authority: &SourceAuthority, stamp: Option<SourceStamp>) -> bool {
        let Some(stamp) = stamp else {
            return false;
        };
        if authority.epoch() != Some(stamp.epoch) {
            return false;
        }
        if self.epoch != Some(stamp.epoch) {
            self.epoch = Some(stamp.epoch);
            self.seq = None;
        }
        if self.seq.is_some_and(|seq| stamp.seq <= seq) {
            return false;
        }
        self.seq = Some(stamp.seq);
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommandIdentity {
    pub owner: usize,
    pub fd: i32,
    pub source: u64,
    pub command: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObservationIdentity {
    command: CommandIdentity,
    observation: u64,
}

pub(crate) struct WatchObservation {
    shared: SharedWatch<ObservationIdentity>,
    eligible: Option<(usize, i32, u64)>,
    eligible_since: Duration,
    next_epoch: Option<u64>,
    frozen: bool,
    authority: Option<SourceAuthority>,
    #[cfg(test)]
    test_seq: u64,
}

impl Default for WatchObservation {
    fn default() -> Self {
        Self {
            shared: SharedWatch::default(),
            eligible: None,
            eligible_since: Duration::ZERO,
            next_epoch: Some(0),
            frozen: false,
            authority: None,
            #[cfg(test)]
            test_seq: 0,
        }
    }
}

impl WatchObservation {
    pub fn freeze(&mut self, frozen: bool) {
        self.frozen = frozen;
    }

    pub fn bind_source(&mut self, authority: Option<SourceAuthority>) {
        if !self.frozen {
            self.authority = authority;
        }
    }

    pub fn reset(&mut self) {
        if self.frozen {
            return;
        }
        self.shared.set_identity(None);
        self.eligible = None;
        // Never reset the epoch allocator: A -> B -> A is a new observation.
    }

    pub fn reset_owner(&mut self, owner: usize) {
        if self.eligible.is_some_and(|identity| identity.0 == owner) {
            self.reset();
        }
    }

    pub fn retire_owner(&mut self, owner: usize) {
        if !self.frozen
            && self
                .shared
                .identity()
                .is_some_and(|id| id.command.owner == owner)
        {
            self.shared.set_identity(None);
        }
    }

    pub fn sync(
        &mut self,
        eligible: Option<(usize, i32, u64)>,
        command: Option<u64>,
        now: Duration,
    ) {
        if self.frozen {
            return;
        }
        self.prepare(eligible, now);
        let identity =
            eligible
                .zip(command)
                .map(|((owner, fd, source), command)| CommandIdentity {
                    owner,
                    fd,
                    source,
                    command,
                });
        match identity {
            None => {
                self.shared.set_identity(None);
            }
            Some(identity) if self.shared.identity().map(|id| id.command) != Some(identity) => {
                self.start(identity, now, false);
            }
            Some(_) => {}
        }
    }

    pub fn prepare(&mut self, eligible: Option<(usize, i32, u64)>, now: Duration) {
        if self.frozen {
            return;
        }
        if self.eligible != eligible {
            self.shared.set_identity(None);
            self.eligible = eligible;
            self.eligible_since = now;
        }
    }

    fn start(&mut self, command: CommandIdentity, now: Duration, trusted: bool) {
        let Some(epoch) = self.next_epoch else {
            self.shared.set_identity(None);
            return;
        };
        self.next_epoch = epoch.checked_add(1);
        let identity = ObservationIdentity {
            command,
            observation: epoch,
        };
        self.shared.set_identity(Some(identity));
        let event = if trusted {
            WatchEvent::BeginTrustedCommand
        } else {
            WatchEvent::AttachRunningObservation
        };
        if self.shared.advance(now, &identity, event).is_err() {
            self.shared.set_identity(None);
        }
    }

    /// Called once after parsing, with the original read time and sequence.
    pub fn batch(
        &mut self,
        command: Option<CommandIdentity>,
        batch: WatchBatch,
        seq: u64,
        times: (Duration, Duration),
        admitted: bool,
        nonempty: bool,
    ) {
        let (read_at, now) = times;
        if self.frozen {
            return;
        }
        let Some(command) =
            command.filter(|id| self.eligible == Some((id.owner, id.fd, id.source)))
        else {
            return;
        };
        let fresh = read_at >= self.eligible_since;
        let clean_start = batch.starts == 1
            && !batch.ended
            && !batch.interrupted
            && !batch.fragmented
            && fresh
            && admitted;
        if self.shared.identity().map(|id| id.command) != Some(command) || batch.interrupted {
            self.start(command, now, clean_start);
        }
        let Some(identity) = self.shared.identity().copied() else {
            return;
        };
        if !nonempty {
            return;
        }
        let disposition = if !fresh
            || !admitted
            || batch.starts > 1
            || (batch.ended && batch.starts > 0)
            || batch.interrupted
            || (batch.starts > 0 && batch.fragmented)
        {
            WatchOutputDisposition::Quarantined
        } else if batch.boundary {
            WatchOutputDisposition::BaselineOnly
        } else {
            WatchOutputDisposition::Accepted
        };
        let _ = self.shared.advance(
            read_at,
            &identity,
            WatchEvent::OutputObserved {
                batch_seq: seq,
                disposition,
            },
        );
    }

    pub fn heartbeat(&mut self, now: Duration) {
        if self.frozen {
            return;
        }
        if let Some(identity) = self.shared.identity().copied() {
            let _ = self.shared.advance(now, &identity, WatchEvent::Advance);
        }
    }

    pub fn rhythm(&self, owner: usize, now: Duration) -> WatchRhythm {
        self.shared
            .identity()
            .filter(|id| id.command.owner == owner)
            .filter(|id| {
                self.authority
                    .as_ref()
                    .is_none_or(|source| source.epoch() == Some(id.command.source))
            })
            .and_then(|id| self.shared.snapshot(now, id).ok())
            .map_or(WatchRhythm::Steady, |snapshot| snapshot.rhythm)
    }

    // Legacy host presentation fixtures use synthetic, explicit test sources.
    #[cfg(test)]
    pub fn observe(&mut self, owner: Option<usize>, running: bool, now: Duration) -> bool {
        let established = self
            .shared
            .identity()
            .is_some_and(|id| Some(id.command.owner) == owner)
            && running;
        self.sync(
            owner.filter(|_| running).map(|id| (id, 1, 1)),
            running.then_some(1),
            now,
        );
        established
    }

    #[cfg(test)]
    pub fn output(&mut self, owner: usize, now: Duration, admitted: bool) {
        let Some(identity) = self
            .shared
            .identity()
            .copied()
            .filter(|id| id.command.owner == owner)
        else {
            return;
        };
        self.test_seq += 1;
        let _ = self.shared.advance(
            now,
            &identity,
            WatchEvent::OutputObserved {
                batch_seq: self.test_seq,
                disposition: if admitted {
                    WatchOutputDisposition::Accepted
                } else {
                    WatchOutputDisposition::BaselineOnly
                },
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
    fn command(n: u64) -> CommandIdentity {
        CommandIdentity {
            owner: 1,
            fd: 7,
            source: 11,
            command: n,
        }
    }
    fn watch() -> WatchObservation {
        let mut watch = WatchObservation::default();
        watch.sync(Some((1, 7, 11)), None, ms(0));
        watch
    }
    fn batch(w: &mut WatchObservation, seq: u64, at: u64) {
        w.batch(
            Some(command(1)),
            WatchBatch::default(),
            seq,
            (ms(at), ms(at + 1)),
            true,
            true,
        );
    }
    #[test]
    fn actual_read_time_is_not_replaced_by_dispatch_time() {
        let mut w = watch();
        w.batch(
            Some(command(1)),
            WatchBatch {
                starts: 1,
                boundary: true,
                ..WatchBatch::default()
            },
            0,
            (ms(10), ms(11)),
            true,
            true,
        );
        for (seq, at) in [(1, 100), (2, 600), (3, 1300)] {
            batch(&mut w, seq, at);
        }
        assert_eq!(w.rhythm(1, ms(1300)), WatchRhythm::Busy);
        assert_eq!(
            w.rhythm(1, ms(1300) + Duration::from_nanos(1)),
            WatchRhythm::Steady
        );
        assert_eq!(w.rhythm(1, ms(4300)), WatchRhythm::Waiting);
        batch(&mut w, 4, 4300);
        assert_eq!(w.rhythm(1, ms(5199)), WatchRhythm::Resumed);
        assert_ne!(w.rhythm(1, ms(5200)), WatchRhythm::Resumed);
    }
    #[test]
    fn duplicate_sequence_and_backwards_fresh_sequence_cannot_replay() {
        let mut w = watch();
        w.sync(Some((1, 7, 11)), Some(1), ms(0));
        batch(&mut w, 1, 10);
        batch(&mut w, 2, 100);
        let before = w.shared.clone();
        batch(&mut w, 2, 9000);
        assert_eq!(w.shared, before);
        batch(&mut w, 3, 90);
        let after_backwards = w.shared.clone();
        batch(&mut w, 3, 5000);
        assert_eq!(w.shared, after_backwards);
    }
    #[test]
    fn restored_owner_quarantines_queued_batches_without_catchup() {
        let mut w = watch();
        w.sync(Some((1, 7, 11)), Some(1), ms(0));
        batch(&mut w, 1, 10);
        w.sync(None, None, ms(500));
        w.sync(Some((1, 7, 11)), Some(1), ms(5000));
        let id = *w.shared.identity().unwrap();
        w.batch(
            Some(command(1)),
            WatchBatch::default(),
            2,
            (ms(400), ms(5010)),
            true,
            true,
        );
        assert_eq!(w.rhythm(1, ms(5010)), WatchRhythm::Steady);
        assert_eq!(*w.shared.identity().unwrap(), id);
        batch(&mut w, 3, 5100); // neutral after quarantine
        batch(&mut w, 4, 5200);
        batch(&mut w, 5, 5300);
        assert_eq!(w.rhythm(1, ms(5300)), WatchRhythm::Steady);
        batch(&mut w, 6, 5400);
        assert_eq!(w.rhythm(1, ms(5400)), WatchRhythm::Busy);
    }
    #[test]
    fn mixed_completion_start_and_fragmented_start_are_neutral() {
        for summary in [
            WatchBatch {
                starts: 2,
                boundary: true,
                ..WatchBatch::default()
            },
            WatchBatch {
                starts: 1,
                boundary: true,
                ended: true,
                ..WatchBatch::default()
            },
            WatchBatch {
                starts: 1,
                boundary: true,
                fragmented: true,
                ..WatchBatch::default()
            },
            WatchBatch {
                interrupted: true,
                boundary: true,
                ..WatchBatch::default()
            },
        ] {
            let mut w = watch();
            w.batch(Some(command(1)), summary, 1, (ms(10), ms(11)), true, true);
            batch(&mut w, 2, 100);
            batch(&mut w, 3, 200);
            batch(&mut w, 4, 300);
            assert_eq!(w.rhythm(1, ms(300)), WatchRhythm::Steady);
            batch(&mut w, 5, 400);
            assert_eq!(w.rhythm(1, ms(400)), WatchRhythm::Busy);
        }
    }
    #[test]
    fn stale_owner_source_and_frozen_output_leave_identity_and_clock_untouched() {
        let mut w = watch();
        w.sync(Some((1, 7, 11)), Some(1), ms(0));
        let before = w.shared.clone();
        for identity in [
            CommandIdentity {
                owner: 2,
                ..command(1)
            },
            CommandIdentity {
                fd: 8,
                ..command(1)
            },
            CommandIdentity {
                source: 12,
                ..command(1)
            },
        ] {
            w.batch(
                Some(identity),
                WatchBatch::default(),
                999,
                (ms(9000), ms(9001)),
                true,
                true,
            );
            assert_eq!(w.shared, before);
        }
        w.freeze(true);
        w.reset();
        w.sync(Some((2, 8, 12)), Some(2), ms(10000));
        w.heartbeat(ms(10000));
        assert_eq!(w.shared, before);
        assert_eq!(w.rhythm(1, ms(10000)), WatchRhythm::Waiting);
    }
    #[test]
    fn reader_rebuild_and_sequence_exhaustion_never_reuse_source() {
        let authority = SourceAuthority::default();
        let mut old = authority.start();
        let at = Instant::now();
        let old_stamp = old.stamp(at).unwrap();
        let mut receiver = SourceReceiver::default();
        assert!(receiver.accept(&authority, Some(old_stamp)));
        assert!(!receiver.accept(&authority, Some(old_stamp)));
        let mut new = authority.start();
        let new_stamp = new.stamp(at).unwrap();
        assert_ne!(old_stamp.epoch, new_stamp.epoch);
        assert!(!receiver.accept(&authority, Some(old_stamp)));
        old.next = Some(u64::MAX);
        assert!(old.stamp(at).is_none());
        assert_eq!(authority.epoch(), Some(new_stamp.epoch));
        assert!(receiver.accept(&authority, Some(new_stamp)));
        new.next = Some(u64::MAX);
        assert!(new.stamp(at).is_none());
        assert!(authority.epoch().is_none());
        assert!(!receiver.accept(&authority, Some(new_stamp)));
    }
    #[test]
    fn old_deadline_rejects_before_clock_and_epoch_exhaustion_stays_closed() {
        use jterm_core::organism_watch::WatchError;
        let mut w = watch();
        w.sync(Some((1, 7, 11)), Some(1), ms(0));
        let old_id = *w.shared.identity().unwrap();
        let token = w
            .shared
            .snapshot(ms(0), &old_id)
            .unwrap()
            .next_deadline
            .unwrap();
        w.reset();
        w.sync(Some((1, 7, 11)), Some(1), ms(10));
        let before = w.shared.clone();
        assert_eq!(
            w.shared.advance(
                ms(9000),
                &old_id,
                WatchEvent::DeadlineDue { deadline: token }
            ),
            Err(WatchError::StaleIdentity)
        );
        assert_eq!(w.shared, before);
        w.reset();
        w.next_epoch = None;
        w.sync(Some((1, 7, 11)), Some(1), ms(100));
        assert!(w.shared.identity().is_none());
    }
    #[test]
    fn old_completion_cannot_attach_or_retire_a_rebuilt_reader() {
        let authority = SourceAuthority::default();
        let mut source_a = authority.start();
        let stamp_a = source_a.stamp(Instant::now()).unwrap();
        let mut receiver = SourceReceiver::default();
        assert!(receiver.accept(&authority, Some(stamp_a)));
        let mut w = WatchObservation::default();
        let eligible_a = Some((1, 7, stamp_a.epoch));
        w.bind_source(Some(authority.clone()));
        w.sync(eligible_a, Some(1), ms(0));
        let before = w.shared.clone();
        w.freeze(true); // Entire intermediate native parsing phase.
        let source_b = authority.start();
        assert_ne!(source_b.epoch, stamp_a.epoch);
        w.retire_owner(1); // Completion from A.
        w.sync(Some((1, 7, source_b.epoch)), Some(2), ms(9000));
        assert_eq!(w.shared, before);
        assert_ne!(authority.epoch(), Some(stamp_a.epoch)); // Final admission rejects.
        w.freeze(false); // Restored even on rejected source / early Task return.
        assert_eq!(w.shared, before);
        assert_eq!(w.rhythm(1, ms(9000)), WatchRhythm::Steady); // Read-only fail closed.
        w.sync(Some((1, 7, source_b.epoch)), Some(2), ms(9000));
        assert_eq!(w.shared.identity().unwrap().command.source, source_b.epoch);
        assert_eq!(w.rhythm(1, ms(9000)), WatchRhythm::Steady);
        let current = w.shared.clone();
        // No old source can be interpreted as output of the new command.
        w.batch(
            Some(CommandIdentity {
                owner: 1,
                fd: 7,
                source: stamp_a.epoch,
                command: 2,
            }),
            WatchBatch {
                starts: 1,
                boundary: true,
                ..WatchBatch::default()
            },
            stamp_a.seq,
            (ms(10), ms(9100)),
            true,
            true,
        );
        assert_eq!(w.shared, current);
    }

    #[test]
    fn source_exhaustion_is_neutral_in_current_read_only_presentation() {
        let authority = SourceAuthority::default();
        let mut writer = authority.start();
        let epoch = writer.epoch;
        let mut w = WatchObservation::default();
        w.bind_source(Some(authority.clone()));
        w.sync(Some((1, 7, epoch)), Some(1), ms(0));
        let id = CommandIdentity {
            owner: 1,
            fd: 7,
            source: epoch,
            command: 1,
        };
        for seq in 0..4 {
            w.batch(
                Some(id),
                WatchBatch::default(),
                seq,
                (ms(seq * 100), ms(seq * 100)),
                true,
                true,
            );
        }
        assert_eq!(w.rhythm(1, ms(300)), WatchRhythm::Busy);
        let before = w.shared.clone();
        writer.next = Some(u64::MAX);
        assert!(writer.stamp(Instant::now()).is_none());
        assert_eq!(w.rhythm(1, ms(300)), WatchRhythm::Steady);
        assert_eq!(w.shared, before);
    }
}
