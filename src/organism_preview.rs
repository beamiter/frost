//! A finite settings-only example sequence. Never produces work events.
use std::sync::Arc;
use std::time::Duration;

use jterm_core::organism_daily::PreviewPose;

const POSES: [PreviewPose; 5] = [
    PreviewPose::Calm,
    PreviewPose::Working,
    PreviewPose::Concerned,
    PreviewPose::Success,
    PreviewPose::Sleeping,
];
const STEP: Duration = Duration::from_secs(2);
const TOTAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DemoStep {
    pub number: usize,
    pub pose: PreviewPose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewAction {
    Play,
    Pause,
    Resume,
    Stop,
    Hello,
}

#[derive(Clone, Debug)]
pub(crate) struct PreviewRequest {
    epoch: Arc<()>,
    pub action: PreviewAction,
}

#[derive(Clone, Copy)]
struct Paused {
    at: Duration,
    elapsed: Duration,
}

pub(crate) struct PreviewSequence {
    started: Option<Duration>,
    paused: Option<Paused>,
    paused_total: Duration,
    control_epoch: Arc<()>,
    action_identities: [Arc<()>; 5],
}

impl Default for PreviewSequence {
    fn default() -> Self {
        Self {
            started: None,
            paused: None,
            paused_total: Duration::ZERO,
            control_epoch: Arc::new(()),
            action_identities: std::array::from_fn(|_| Arc::new(())),
        }
    }
}

impl PreviewSequence {
    pub fn control_epoch(&self) -> Arc<()> {
        Arc::clone(&self.control_epoch)
    }

    pub fn action_identity(&self, action: PreviewAction) -> Arc<()> {
        Arc::clone(&self.action_identities[action as usize])
    }

    pub fn request(&self, action: PreviewAction) -> PreviewRequest {
        PreviewRequest {
            epoch: self.control_epoch(),
            action,
        }
    }

    pub fn accepts(&self, request: &PreviewRequest, now: Duration) -> bool {
        if !Arc::ptr_eq(&request.epoch, &self.control_epoch) {
            return false;
        }
        let active = self.sample(now).is_some();
        match request.action {
            PreviewAction::Play | PreviewAction::Hello => !active,
            PreviewAction::Pause => active && self.paused.is_none(),
            PreviewAction::Resume => active && self.paused.is_some(),
            PreviewAction::Stop => active,
        }
    }

    pub fn retire_controls(&mut self) {
        self.control_epoch = Arc::new(());
    }

    /// Repeated Play cannot extend a running or paused sequence.
    pub fn start(&mut self, now: Duration) -> bool {
        if self.sample(now).is_some() || now.checked_add(TOTAL).is_none() {
            return false;
        }
        self.started = Some(now);
        self.paused = None;
        self.paused_total = Duration::ZERO;
        self.retire_controls();
        true
    }

    pub fn stop(&mut self) {
        self.started = None;
        self.paused = None;
        self.paused_total = Duration::ZERO;
        self.retire_controls();
    }

    fn elapsed(&self, now: Duration) -> Option<Duration> {
        if let Some(paused) = self.paused {
            now.checked_sub(paused.at)?;
            Some(paused.elapsed)
        } else {
            now.checked_sub(self.started?)?
                .checked_sub(self.paused_total)
        }
    }

    pub fn pause(&mut self, now: Duration) -> bool {
        if self.paused.is_some() || self.sample(now).is_none() {
            return false;
        }
        let Some(elapsed) = self.elapsed(now) else {
            return false;
        };
        self.paused = Some(Paused { at: now, elapsed });
        self.retire_controls();
        true
    }

    pub fn resume(&mut self, now: Duration) -> bool {
        let Some(paused) = self.paused else {
            return false;
        };
        let Some(total) = now
            .checked_sub(paused.at)
            .and_then(|delta| self.paused_total.checked_add(delta))
        else {
            return false;
        };
        if self
            .started
            .and_then(|start| start.checked_add(total)?.checked_add(TOTAL))
            .is_none()
        {
            return false;
        }
        self.paused_total = total;
        self.paused = None;
        self.retire_controls();
        true
    }

    pub fn is_paused(&self) -> bool {
        self.paused.is_some()
    }

    /// Same monotonic animation clock, with all paused duration excluded.
    pub fn presentation_time(&self, now: Duration) -> Duration {
        self.paused
            .map_or(now, |paused| paused.at)
            .saturating_sub(self.paused_total)
    }

    pub fn sample(&self, now: Duration) -> Option<DemoStep> {
        let elapsed = self.elapsed(now)?;
        if elapsed >= TOTAL {
            return None;
        }
        let index = (elapsed.as_secs() / STEP.as_secs()) as usize;
        Some(DemoStep {
            number: index + 1,
            pose: POSES[index],
        })
    }

    /// One absolute next boundary, including final return; paused has no wake.
    pub fn next_wake(&self, now: Duration) -> Option<Duration> {
        if self.paused.is_some() {
            return None;
        }
        let step = self.sample(now)?;
        self.started?
            .checked_add(self.paused_total)?
            .checked_add(STEP * step.number as u32)
    }
}

pub(crate) fn preview_visible(focused: bool, settings_open: bool, editor_open: bool) -> bool {
    focused && settings_open && !editor_open
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn pause_freezes_phase_frame_and_exact_remaining_time_without_wakes() {
        let mut demo = PreviewSequence::default();
        assert!(demo.start(ms(1000)));
        assert!(demo.pause(ms(3500)));
        assert!(!demo.pause(ms(4000)));
        for now in [3500, 4000, 100000] {
            assert_eq!(
                demo.sample(ms(now)),
                Some(DemoStep {
                    number: 2,
                    pose: PreviewPose::Working
                })
            );
            assert_eq!(demo.presentation_time(ms(now)), ms(3500));
            assert_eq!(demo.next_wake(ms(now)), None);
            assert!(!demo.start(ms(now)));
        }
        assert!(demo.resume(ms(100000)));
        assert!(!demo.resume(ms(100001)));
        assert_eq!(demo.presentation_time(ms(100000)), ms(3500));
        assert_eq!(demo.next_wake(ms(100000)), Some(ms(101500)));
        assert_eq!(demo.sample(ms(101499)).unwrap().number, 2);
        assert_eq!(demo.sample(ms(101500)).unwrap().number, 3);
        assert_eq!(demo.sample(ms(107500)), None);
        assert!(!demo.pause(ms(107500)));
        assert!(!demo.resume(ms(107500)));
    }

    #[test]
    fn controls_are_action_and_revision_bound_without_rotating_on_frame_reads() {
        fn assert_clone_send<T: Clone + Send>() {}
        assert_clone_send::<PreviewRequest>();
        let mut demo = PreviewSequence::default();
        let hello = demo.request(PreviewAction::Hello);
        let play = demo.request(PreviewAction::Play);
        assert!(demo.accepts(&play, ms(0)));
        assert!(demo.start(ms(0)));
        assert!(!demo.accepts(&play, ms(0)));
        let pause = demo.request(PreviewAction::Pause);
        let epoch = demo.control_epoch();
        for time in [0, 500, 2000] {
            assert!(demo.sample(ms(time)).is_some());
            demo.presentation_time(ms(time));
            assert!(Arc::ptr_eq(&epoch, &demo.control_epoch()));
        }
        assert!(demo.accepts(&pause, ms(2000)));
        assert!(demo.pause(ms(2000)));
        let resume = demo.request(PreviewAction::Resume);
        assert!(demo.accepts(&resume, ms(5000)));
        assert!(!demo.accepts(&demo.request(PreviewAction::Hello), ms(5000)));
        assert!(demo.resume(ms(5000)));
        assert!(!demo.accepts(&pause, ms(5001)));
        assert!(!demo.accepts(&resume, ms(5001)));
        let stop = demo.request(PreviewAction::Stop);
        demo.stop();
        assert!(!demo.accepts(&stop, ms(5001)));
        assert!(!demo.accepts(&hello, ms(5001)));
        assert!(demo.start(ms(6000)));
        assert!(!demo.accepts(&stop, ms(6001)));
    }

    #[test]
    fn natural_end_rejects_stale_stop_and_action_identities_separate_play() {
        let mut demo = PreviewSequence::default();
        demo.start(ms(0));
        let stop = demo.request(PreviewAction::Stop);
        let epoch = demo.control_epoch();
        assert!(!demo.accepts(&stop, ms(10000)));
        assert!(!demo.accepts(&demo.request(PreviewAction::Pause), ms(10000)));
        assert!(demo.accepts(&demo.request(PreviewAction::Play), ms(10000)));
        assert!(Arc::ptr_eq(&epoch, &demo.control_epoch()));
        assert!(!Arc::ptr_eq(
            &demo.action_identity(PreviewAction::Stop),
            &demo.action_identity(PreviewAction::Play)
        ));
        demo.start(ms(10000));
        assert!(!demo.accepts(&stop, ms(10001)));
    }

    #[test]
    fn demo_has_five_exact_steps_and_a_final_boundary_without_looping() {
        let mut demo = PreviewSequence::default();
        assert!(demo.start(ms(0)));
        for (time, number, pose, next) in [
            (0, 1, PreviewPose::Calm, 2000),
            (1999, 1, PreviewPose::Calm, 2000),
            (2000, 2, PreviewPose::Working, 4000),
            (4000, 3, PreviewPose::Concerned, 6000),
            (6000, 4, PreviewPose::Success, 8000),
            (8000, 5, PreviewPose::Sleeping, 10000),
            (9999, 5, PreviewPose::Sleeping, 10000),
        ] {
            assert_eq!(demo.sample(ms(time)), Some(DemoStep { number, pose }));
            assert_eq!(demo.next_wake(ms(time)), Some(ms(next)));
        }
        for time in [10000, 10001, 1000000] {
            assert_eq!(demo.sample(ms(time)), None);
            assert_eq!(demo.next_wake(ms(time)), None);
        }
    }

    #[test]
    fn stop_and_duplicate_play_preserve_finite_absolute_deadlines() {
        let mut demo = PreviewSequence::default();
        demo.start(ms(100));
        assert!(!demo.start(ms(500)));
        assert_eq!(demo.next_wake(ms(500)), Some(ms(2100)));
        demo.stop();
        assert_eq!(demo.sample(ms(1000)), None);
        assert_eq!(demo.next_wake(ms(5000)), None);
        assert!(demo.start(ms(2000)));
        assert_eq!(demo.next_wake(ms(2000)), Some(ms(4000)));
        assert_eq!(demo.sample(ms(12000)), None);
        assert!(demo.start(ms(12000)));
        assert!(!demo.start(Duration::MAX));
    }

    #[test]
    fn queued_preview_requests_need_the_same_visible_focus_gate() {
        for focused in [false, true] {
            for settings in [false, true] {
                for editor in [false, true] {
                    assert_eq!(
                        preview_visible(focused, settings, editor),
                        focused && settings && !editor
                    );
                }
            }
        }
    }
}
