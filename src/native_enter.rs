//! Window-scoped physical Enter ownership before Iced drops synthetic events.
use iced::window::Id;
use iced_winit::winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use iced_winit::winit::{
    event::WindowEvent,
    keyboard::{Key, KeyCode, NamedKey, PhysicalKey},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub(crate) type SharedOwnership = Arc<parking_lot::Mutex<NativeEnterOwnership>>;

pub(crate) fn install() -> SharedOwnership {
    let state = Arc::new(parking_lot::Mutex::new(NativeEnterOwnership::default()));
    let observer = Arc::clone(&state);
    assert!(
        iced_winit::native_events::install(move |id, event| { observer.lock().observe(id, event) }),
        "native keyboard observer already installed"
    );
    state
}

#[derive(Default)]
pub(crate) struct NativeEnterOwnership {
    windows: HashMap<Id, WindowEnterState>,
    focused: Option<Id>,
}

impl NativeEnterOwnership {
    pub(crate) fn claim_held(&mut self) {
        if let Some(state) = self.focused.and_then(|id| self.windows.get_mut(&id)) {
            state.native.claim_pressed();
        }
    }

    pub(crate) fn claim_pressed(&mut self) {
        // A queued click can be delivered after focus left Frost's sole window.
        let target = self
            .focused
            .or_else(|| (self.windows.len() == 1).then(|| *self.windows.keys().next().unwrap()));
        if let Some(state) = target.and_then(|id| self.windows.get_mut(&id)) {
            state.claim_pressed();
        }
    }

    pub(crate) fn acknowledge_captured(&mut self, id: Id, event: &iced::keyboard::Event) -> bool {
        if self.acknowledge(id, event) {
            // This older delivery was already claimed. Claiming again could
            // absorb a fresh press appended after its real physical release.
            return true;
        }
        self.claim_pressed();
        false
    }

    pub(crate) fn acknowledge(&mut self, id: Id, event: &iced::keyboard::Event) -> bool {
        let iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
            physical_key,
            ..
        } = event
        else {
            return false;
        };
        let Some(state) = self.windows.get_mut(&id) else {
            return false;
        };
        let Some(key) = state
            .pending
            .keys()
            .copied()
            .find(|key| iced_winit::conversion::physical_key(*key) == *physical_key)
        else {
            return false;
        };
        state.acknowledge(key)
    }

    fn observe(&mut self, id: Id, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::Focused(true) => self.focused = Some(id),
            WindowEvent::Focused(false) if self.focused == Some(id) => self.focused = None,
            WindowEvent::Destroyed => {
                self.windows.remove(&id);
                if self.focused == Some(id) {
                    self.focused = None;
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if event.state.is_pressed() {
                    self.focused = Some(id);
                }
                let logical_enter =
                    matches!(event.key_without_modifiers(), Key::Named(NamedKey::Enter));
                let is_enter = logical_enter
                    || matches!(
                        event.physical_key,
                        PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter)
                    );
                let state = self.windows.entry(id).or_default();
                let consumed = state.native.transition(
                    event.physical_key,
                    event.state.is_pressed(),
                    *is_synthetic,
                    is_enter,
                );
                // Iced delivers widget messages before subscription key messages.
                // Keep a count until each forwarded Enter press is acknowledged,
                // even if native key-up arrived before that delivery.
                if !is_synthetic && event.state.is_pressed() && logical_enter && !consumed {
                    let pending = state.pending.entry(event.physical_key).or_default();
                    *pending = pending.saturating_add(1);
                }
                return consumed;
            }
            _ => {}
        }
        false
    }
}

#[derive(Default)]
struct WindowEnterState {
    native: EnterState,
    pending: HashMap<PhysicalKey, usize>,
    pending_owned: HashMap<PhysicalKey, usize>,
}

impl WindowEnterState {
    fn claim_pressed(&mut self) {
        self.native.claim_pressed();
        self.pending_owned
            .extend(self.pending.iter().map(|(key, count)| (*key, *count)));
    }

    fn acknowledge(&mut self, key: PhysicalKey) -> bool {
        let owned = if let Some(count) = self.pending_owned.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                self.pending_owned.remove(&key);
            }
            true
        } else {
            false
        };
        if let Some(count) = self.pending.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                self.pending.remove(&key);
                self.pending_owned.remove(&key);
            }
        }
        owned
    }
}

#[derive(Default)]
struct EnterState {
    pressed: HashSet<PhysicalKey>,
    owned: HashSet<PhysicalKey>,
}

impl EnterState {
    fn claim_pressed(&mut self) {
        self.owned.extend(self.pressed.iter().copied());
    }

    fn transition(
        &mut self,
        key: PhysicalKey,
        pressed: bool,
        synthetic: bool,
        is_enter: bool,
    ) -> bool {
        if synthetic {
            // A positive focus snapshot proves the key is currently held.
            // An emulated release does not prove it was physically released.
            if pressed && is_enter {
                self.pressed.insert(key);
                if !self.owned.is_empty() {
                    self.owned.insert(key);
                }
            }
            return false;
        }
        if !pressed {
            self.pressed.remove(&key);
            self.owned.remove(&key);
            return false;
        }
        if !is_enter && !self.pressed.contains(&key) {
            return false;
        }
        self.pressed.insert(key);
        if !self.owned.is_empty() {
            self.owned.insert(key);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MAIN: PhysicalKey = PhysicalKey::Code(KeyCode::Enter);
    const PAD: PhysicalKey = PhysicalKey::Code(KeyCode::NumpadEnter);

    #[test]
    fn outside_origin_snapshot_stays_passive_until_recall() {
        for key in [MAIN, PAD] {
            let mut state = EnterState::default();
            assert!(!state.transition(key, true, true, true));
            assert!(state.pressed.contains(&key));
            assert!(state.owned.is_empty());
            state.claim_pressed();
            assert!(state.transition(key, true, false, true));
            assert!(!state.transition(key, false, false, true));
            assert!(!state.transition(key, true, false, true));
        }
    }

    #[test]
    fn synthetic_release_never_ends_ownership() {
        let mut state = EnterState::default();
        state.transition(MAIN, true, false, true);
        state.claim_pressed();
        assert!(!state.transition(MAIN, false, true, true));
        assert!(state.transition(MAIN, true, false, true));
        assert!(!state.transition(MAIN, false, false, true));
        assert!(!state.transition(MAIN, true, false, true));
    }

    #[test]
    fn synthetic_partner_joins_existing_owned_gesture_before_partial_release() {
        for (old, inherited) in [(MAIN, PAD), (PAD, MAIN)] {
            let mut state = EnterState::default();
            state.transition(old, true, false, true);
            state.claim_pressed();
            assert!(!state.transition(inherited, true, true, true));
            assert!(!state.transition(old, false, false, true));
            assert!(state.transition(inherited, true, false, true));
            assert!(!state.transition(inherited, false, false, true));
            assert!(!state.transition(inherited, true, false, true));
        }
    }

    #[test]
    fn overlapping_keys_release_independently_in_both_orders() {
        for (first, second) in [(MAIN, PAD), (PAD, MAIN)] {
            for release_first in [true, false] {
                let mut state = EnterState::default();
                state.transition(first, true, false, true);
                state.claim_pressed();
                assert!(state.transition(second, true, false, true));
                let (released, held) = if release_first {
                    (first, second)
                } else {
                    (second, first)
                };
                assert!(!state.transition(released, false, false, true));
                assert!(state.transition(held, true, false, true));
                assert!(!state.transition(held, false, false, true));
                assert!(!state.transition(first, true, false, true));
            }
        }
    }

    #[test]
    fn coalesced_partial_release_still_adopts_the_remaining_key() {
        let mut state = EnterState::default();
        state.transition(MAIN, true, false, true);
        state.transition(PAD, true, false, true);
        state.transition(MAIN, false, false, true);
        state.claim_pressed();
        assert!(state.transition(PAD, true, false, true));
        assert!(!state.transition(PAD, false, false, true));
    }

    #[test]
    fn unrelated_keys_and_layout_changed_release_pass() {
        let mut state = EnterState::default();
        state.transition(MAIN, true, false, true);
        state.claim_pressed();
        let letter = PhysicalKey::Code(KeyCode::KeyA);
        assert!(!state.transition(letter, true, false, false));
        assert!(!state.transition(letter, false, false, false));
        assert!(!state.transition(MAIN, false, false, false));
        assert!(state.owned.is_empty());
    }

    #[test]
    fn claims_are_window_scoped_and_blur_preserves_existing_owner() {
        let first = Id::unique();
        let second = Id::unique();
        let mut tracker = NativeEnterOwnership::default();
        tracker
            .windows
            .entry(first)
            .or_default()
            .native
            .transition(MAIN, true, true, true);
        tracker
            .windows
            .entry(second)
            .or_default()
            .native
            .transition(PAD, true, true, true);
        tracker.observe(first, &WindowEvent::Focused(true));
        tracker.claim_pressed();
        assert!(tracker.windows[&first].native.owned.contains(&MAIN));
        assert!(tracker.windows[&second].native.owned.is_empty());
        tracker.observe(first, &WindowEvent::Focused(false));
        assert!(tracker.windows[&first].native.owned.contains(&MAIN));
        tracker.observe(second, &WindowEvent::Focused(true));
        tracker.claim_pressed();
        assert!(tracker.windows[&second].native.owned.contains(&PAD));
        tracker.observe(first, &WindowEvent::Destroyed);
        assert!(!tracker.windows.contains_key(&first));
        assert_eq!(tracker.focused, Some(second));
    }
    #[test]
    fn queued_presses_remain_claimable_after_native_release() {
        let mut state = WindowEnterState::default();
        state.native.transition(MAIN, true, false, true);
        state.pending.insert(MAIN, 27);
        state.native.transition(MAIN, false, false, true);
        state.claim_pressed();
        for _ in 0..27 {
            assert!(state.acknowledge(MAIN));
        }
        assert!(state.pending.is_empty());
        assert!(state.pending_owned.is_empty());
        assert!(state.native.owned.is_empty());
        assert!(
            !state.native.transition(MAIN, true, false, true),
            "fresh physical press still passes after delivery drains"
        );
    }

    #[test]
    fn fresh_press_after_release_is_not_added_to_owned_delivery_prefix() {
        for key in [MAIN, PAD] {
            let mut state = WindowEnterState::default();
            state.native.transition(key, true, false, true);
            state.pending.insert(key, 2);
            state.claim_pressed();
            state.native.transition(key, false, false, true);
            assert!(!state.native.transition(key, true, false, true));
            *state.pending.get_mut(&key).unwrap() += 1;
            assert!(state.acknowledge(key));
            assert!(state.acknowledge(key));
            assert!(!state.acknowledge(key));
            assert!(state.pending.is_empty());
            assert!(state.pending_owned.is_empty());
        }
    }

    #[test]
    fn captured_owned_delivery_does_not_reclaim_a_later_fresh_press() {
        for key in [MAIN, PAD] {
            let id = Id::unique();
            let mut tracker = NativeEnterOwnership {
                focused: Some(id),
                ..NativeEnterOwnership::default()
            };
            let state = tracker.windows.entry(id).or_default();
            state.native.transition(key, true, false, true);
            state.pending.insert(key, 1);
            state.claim_pressed();
            state.native.transition(key, false, false, true);
            state.native.transition(key, true, false, true);
            *state.pending.get_mut(&key).unwrap() += 1;
            let event = iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                physical_key: iced_winit::conversion::physical_key(key),
                location: if key == PAD {
                    iced::keyboard::Location::Numpad
                } else {
                    iced::keyboard::Location::Standard
                },
                modifiers: iced::keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            };
            assert!(tracker.acknowledge_captured(id, &event));
            assert!(!tracker.acknowledge(id, &event));
            assert!(tracker.windows[&id].native.owned.is_empty());
            assert!(tracker.windows[&id].pending.is_empty());
        }
    }

    #[test]
    fn visible_surface_does_not_claim_a_queued_fresh_confirmation() {
        let id = Id::unique();
        let mut tracker = NativeEnterOwnership {
            focused: Some(id),
            ..NativeEnterOwnership::default()
        };
        let state = tracker.windows.entry(id).or_default();
        state.pending.insert(MAIN, 1);
        // A query edit can refresh subscriptions before its coalesced Enter is
        // delivered. Surface visibility alone must leave that press usable.
        tracker.claim_held();
        assert!(!tracker.windows.get_mut(&id).unwrap().acknowledge(MAIN));
    }

    #[test]
    fn unclaimed_queued_terminal_input_is_unchanged() {
        let mut state = WindowEnterState::default();
        state.pending.insert(MAIN, 3);
        for _ in 0..3 {
            assert!(!state.acknowledge(MAIN));
        }
        assert!(state.pending.is_empty());
    }
}
