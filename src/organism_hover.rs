//! Pure, volatile pointer greeting. No terminal, widgets, persistence or I/O.
use std::time::Duration;

use jterm_core::organism::RenderContext;
use jterm_core::organism_daily::GentleInteraction;

const DWELL: Duration = Duration::from_millis(600);

#[derive(Default)]
pub(crate) struct LiveGreeting {
    interaction: GentleInteraction,
    // Keep physical entry separate from cancellation: a stationary pointer
    // must never re-arm itself after busy work, typing, or a cooldown.
    inside: bool,
    epoch: u64,
    owner: Option<usize>,
    entry_epoch: Option<u64>,
    candidate: Option<Duration>,
    active_until: Option<Duration>,
}

impl LiveGreeting {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn enter(
        &mut self,
        id: usize,
        epoch: u64,
        now: Duration,
        context: RenderContext,
        eligible: bool,
    ) {
        if epoch != self.epoch || self.inside {
            return;
        }
        self.inside = true;
        self.owner = Some(id);
        self.entry_epoch = Some(epoch);
        if eligible && self.interaction.clone().request(now, context) {
            self.candidate = Some(now.saturating_add(DWELL));
        }
    }

    pub fn cancel(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.candidate = None;
        self.active_until = None;
        self.interaction.cancel();
    }

    pub fn cancel_owner(&mut self, id: usize) {
        if self.owner == Some(id) {
            self.cancel();
        }
    }

    pub fn exit_reference(&self) -> Option<(usize, u64)> {
        self.owner.zip(self.entry_epoch)
    }

    pub fn exit(&mut self, id: usize, epoch: u64) {
        if self.exit_reference() == Some((id, epoch)) {
            self.leave();
        }
    }

    pub fn leave(&mut self) {
        self.cancel();
        self.inside = false;
        self.owner = None;
        self.entry_epoch = None;
    }

    pub fn advance(
        &mut self,
        owner: Option<usize>,
        now: Duration,
        context: RenderContext,
        eligible: bool,
    ) {
        if !eligible
            || owner.is_none()
            || owner != self.owner
            || !GentleInteraction::default().request(now, context)
        {
            self.cancel();
            return;
        }
        if self.candidate.is_some_and(|deadline| now >= deadline) {
            self.candidate = None;
            if self.interaction.request(now, context) {
                self.active_until = Some(now.saturating_add(GentleInteraction::HOLD));
            }
        }
        if self.active_until.is_some_and(|deadline| now >= deadline) {
            self.active_until = None;
            self.interaction.cancel();
        }
    }

    pub fn deadline(&self, now: Duration) -> Option<Duration> {
        // A due candidate must still emit once: rendering alone cannot admit
        // a greeting, and an unrelated redraw must not lose its pending wake.
        self.candidate
            .or(self.active_until.filter(|deadline| *deadline > now))
    }

    pub fn apply(&self, id: usize, now: Duration, context: RenderContext) -> RenderContext {
        if self.owner == Some(id) && self.active_until.is_some_and(|deadline| now < deadline) {
            self.interaction.clone().apply(now, context)
        } else {
            context
        }
    }
}

/// Event bookkeeping only; it does not capture or consume the native event.
pub(crate) fn update_pressed_button<B: PartialEq>(buttons: &mut Vec<B>, button: B, pressed: bool) {
    if pressed {
        if !buttons.contains(&button) {
            buttons.push(button);
        }
    } else {
        buttons.retain(|current| *current != button);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::organism::Behavior;
    use jterm_core::organism_daily::PreviewPose;

    fn idle() -> RenderContext {
        PreviewPose::Calm.context()
    }

    #[test]
    fn continuous_entry_accepts_once_at_600ms_and_expires_at_two_seconds() {
        let mut hello = LiveGreeting::default();
        hello.enter(1, hello.epoch(), Duration::ZERO, idle(), true);
        assert_eq!(hello.deadline(Duration::ZERO), Some(DWELL));
        hello.enter(1, hello.epoch(), Duration::from_millis(300), idle(), true);
        hello.advance(Some(1), Duration::from_millis(599), idle(), true);
        assert_eq!(hello.apply(1, Duration::from_millis(599), idle()), idle());
        // A redraw after the due time cannot accidentally lose the wake.
        assert_eq!(hello.deadline(Duration::from_millis(601)), Some(DWELL));
        hello.advance(Some(1), DWELL, idle(), true);
        assert_eq!(
            hello.apply(1, DWELL, idle()).behavior,
            PreviewPose::Greeting.context().behavior
        );
        let end = DWELL + GentleInteraction::HOLD;
        assert_eq!(hello.deadline(DWELL), Some(end));
        hello.advance(Some(1), end, idle(), true);
        assert_eq!(hello.apply(1, end, idle()), idle());
        assert_eq!(hello.deadline(end), None);
        hello.advance(Some(1), Duration::from_secs(20), idle(), true);
        hello.enter(1, hello.epoch(), Duration::from_secs(20), idle(), true);
        assert_eq!(hello.deadline(Duration::from_secs(20)), None);
    }

    #[test]
    fn cancelled_stationary_entry_never_rearms_after_busy_or_owner_restoration() {
        for next_owner in [None, Some(2), Some(1)] {
            let mut hello = LiveGreeting::default();
            hello.enter(1, hello.epoch(), Duration::ZERO, idle(), true);
            hello.advance(next_owner, Duration::from_millis(100), idle(), false);
            hello.advance(Some(1), Duration::from_secs(1), idle(), true);
            hello.enter(1, hello.epoch(), Duration::from_secs(1), idle(), true);
            assert_eq!(hello.deadline(Duration::from_secs(1)), None);
            assert_eq!(hello.apply(1, Duration::from_secs(1), idle()), idle());
            hello.leave();
            hello.enter(1, hello.epoch(), Duration::from_secs(1), idle(), true);
            assert_eq!(
                hello.deadline(Duration::from_secs(1)),
                Some(Duration::from_millis(1600))
            );
        }
        let mut hello = LiveGreeting::default();
        hello.enter(1, hello.epoch(), Duration::ZERO, idle(), true);
        hello.advance(Some(2), Duration::from_millis(100), idle(), true);
        hello.advance(Some(1), Duration::from_secs(1), idle(), true);
        assert_eq!(hello.deadline(Duration::from_secs(1)), None);
    }

    #[test]
    fn busy_static_hidden_and_press_cancellation_do_not_offer_attention() {
        for behavior in [
            Behavior::WatchCommand,
            Behavior::InspectError,
            Behavior::SitNearError,
            Behavior::UnknownOutcome,
            Behavior::CelebrateBig,
            Behavior::RestAfterPush,
        ] {
            let context = RenderContext { behavior, ..idle() };
            let mut hello = LiveGreeting::default();
            hello.enter(1, hello.epoch(), Duration::ZERO, context, true);
            assert_eq!(hello.deadline(Duration::ZERO), None);
        }
        let mut hello = LiveGreeting::default();
        hello.enter(1, hello.epoch(), Duration::ZERO, idle(), false);
        hello.advance(Some(1), Duration::from_secs(1), idle(), true);
        assert_eq!(hello.deadline(Duration::from_secs(1)), None);
        hello.leave();
        hello.enter(1, hello.epoch(), Duration::from_secs(1), idle(), true);
        hello.cancel();
        hello.advance(Some(1), Duration::from_secs(2), idle(), true);
        assert_eq!(hello.deadline(Duration::from_secs(2)), None);
    }

    #[test]
    fn cooldown_survives_cancellation_owner_change_and_reentry_without_looping() {
        let mut hello = LiveGreeting::default();
        hello.enter(1, hello.epoch(), Duration::ZERO, idle(), true);
        hello.advance(Some(1), DWELL, idle(), true);
        hello.cancel();
        hello.leave();
        hello.enter(2, hello.epoch(), Duration::from_secs(1), idle(), true);
        assert_eq!(hello.deadline(Duration::from_secs(1)), None);
        hello.advance(Some(2), Duration::from_secs(9), idle(), true);
        assert_eq!(hello.deadline(Duration::from_secs(9)), None);
        hello.leave();
        hello.enter(2, hello.epoch(), Duration::from_secs(9), idle(), true);
        hello.advance(Some(2), Duration::from_millis(9600), idle(), true);
        assert_ne!(hello.apply(2, Duration::from_millis(9600), idle()), idle());
        assert_eq!(hello.apply(1, Duration::from_millis(9600), idle()), idle());
    }

    #[test]
    fn stale_entry_callback_cannot_rearm_after_owner_changes_or_leave() {
        let mut hello = LiveGreeting::default();
        let old = hello.epoch();
        hello.enter(1, old, Duration::ZERO, idle(), true);
        hello.cancel();
        hello.leave();
        hello.enter(1, old, Duration::from_secs(1), idle(), true);
        assert_eq!(hello.deadline(Duration::from_secs(1)), None);
        hello.enter(1, hello.epoch(), Duration::from_secs(1), idle(), true);
        assert!(hello.deadline(Duration::from_secs(1)).is_some());
    }

    #[test]
    fn exits_are_bound_to_the_entry_and_survive_cancellation_without_sticking() {
        let mut hello = LiveGreeting::default();
        let old = hello.epoch();
        hello.enter(1, old, Duration::ZERO, idle(), true);
        hello.cancel();
        // Cancellation invalidates new entry callbacks, not the physical exit.
        hello.exit(1, old);
        let current = hello.epoch();
        hello.enter(2, current, Duration::from_secs(1), idle(), true);
        hello.exit(1, old);
        assert!(hello.deadline(Duration::from_secs(1)).is_some());
        hello.exit(2, current);
        assert_eq!(hello.deadline(Duration::from_secs(1)), None);
    }

    #[test]
    fn observed_buttons_track_multiple_presses_without_duplicates() {
        let mut buttons = Vec::new();
        update_pressed_button(&mut buttons, 1, true);
        update_pressed_button(&mut buttons, 1, true);
        update_pressed_button(&mut buttons, 2, true);
        assert_eq!(buttons, vec![1, 2]);
        update_pressed_button(&mut buttons, 1, false);
        assert_eq!(buttons, vec![2]);
        update_pressed_button(&mut buttons, 2, false);
        assert!(buttons.is_empty());
        update_pressed_button(&mut buttons, 3, true);
        buttons.clear(); // Native blur/leave revokes stale observed button state.
        assert!(buttons.is_empty());
    }
}
