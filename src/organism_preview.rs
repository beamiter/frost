//! A finite settings-only example sequence. Never produces work events.
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

#[derive(Default)]
pub(crate) struct PreviewSequence {
    started: Option<Duration>,
}

impl PreviewSequence {
    /// Repeated Play cannot extend a sequence already in progress.
    pub fn start(&mut self, now: Duration) -> bool {
        if self.sample(now).is_some() || now.checked_add(TOTAL).is_none() {
            return false;
        }
        self.started = Some(now);
        true
    }

    pub fn stop(&mut self) {
        self.started = None;
    }

    pub fn sample(&self, now: Duration) -> Option<DemoStep> {
        let elapsed = now.checked_sub(self.started?)?;
        if elapsed >= TOTAL {
            return None;
        }
        let index = (elapsed.as_secs() / STEP.as_secs()) as usize;
        Some(DemoStep {
            number: index + 1,
            pose: POSES[index],
        })
    }

    /// One absolute next boundary, including the final return to manual pose.
    pub fn next_wake(&self, now: Duration) -> Option<Duration> {
        let step = self.sample(now)?;
        self.started?.checked_add(STEP * step.number as u32)
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
