//! Window IME events have no pane identity. Bind each composition to its first
//! terminal owner and cancel it when focus leaves, rather than replaying a late
//! commit into either the new pane or a hidden old pane.
use iced::{advanced::input_method::Event, keyboard};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct Owner {
    pub session: usize,
    pub instance: Arc<()>,
}
impl PartialEq for Owner {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session && Arc::ptr_eq(&self.instance, &other.instance)
    }
}
impl Eq for Owner {}

#[derive(Default)]
enum Composition {
    #[default]
    Idle,
    Active,
    Cancelled {
        closed: bool,
    },
}

#[derive(Default)]
pub(crate) struct Ownership {
    owner: Option<Owner>,
    composition: Composition,
    cancelled_enter_claimed: bool,
}

impl Ownership {
    /// Called before and after every reducer, including early-return handlers.
    /// Return true when old terminal preedit must be removed from display.
    pub fn sync(&mut self, owner: Option<Owner>) -> bool {
        if self.owner == owner {
            return false;
        }
        self.owner = owner;
        let active = matches!(self.composition, Composition::Active);
        if active {
            self.composition = Composition::Cancelled { closed: false };
            self.cancelled_enter_claimed = false;
        }
        active
    }

    /// Admit only a current-owner lifecycle. Empty preedit is commonly emitted
    /// immediately before Commit, so it must not relinquish composition ownership.
    pub fn admit(&mut self, event: &Event) -> bool {
        if self.owner.is_none() {
            // An overlay may receive the cancelled lifecycle's last events.
            // Retire them without ever treating their text as terminal input.
            match event {
                Event::Commit(_) => self.retire_cancelled_commit(),
                Event::Closed => {
                    if matches!(self.composition, Composition::Cancelled { .. }) {
                        self.composition = Composition::Cancelled { closed: true };
                    }
                }
                _ => {}
            }
            return false;
        }
        if let Composition::Cancelled { closed } = &self.composition {
            match event {
                Event::Opened => self.composition = Composition::Idle,
                Event::Closed => {
                    self.composition = Composition::Cancelled { closed: true };
                    return false;
                }
                Event::Preedit(text, _) if *closed && !text.is_empty() => {
                    self.composition = Composition::Active;
                }
                Event::Commit(_) => {
                    // Retire exactly the cancelled cycle. A subsequent commit-
                    // only platform input is legitimate and must remain usable.
                    self.composition = Composition::Idle;
                    return false;
                }
                _ => return false,
            }
        }
        match event {
            Event::Preedit(text, _) if !text.is_empty() => self.composition = Composition::Active,
            Event::Commit(_) | Event::Closed => self.composition = Composition::Idle,
            _ => {}
        }
        true
    }

    pub fn retire_cancelled_commit(&mut self) {
        if matches!(self.composition, Composition::Cancelled { .. }) {
            self.composition = Composition::Idle;
        }
    }

    /// Active or cancelled candidate confirmation belongs to IME, including
    /// while a new modal owns input. The existing native Enter tracker owns
    /// repeats until physical release; do not add an Iced-only key-up latch.
    pub fn claim_enter(&mut self, event: &keyboard::Event) -> bool {
        if !matches!(
            event,
            keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Enter),
                ..
            }
        ) {
            return false;
        }
        match self.composition {
            Composition::Active => true,
            Composition::Cancelled { closed: false } if !self.cancelled_enter_claimed => {
                // The platform may cancel composition without another IME
                // event. Claim at most one candidate press; native physical
                // ownership handles its repeats. A fresh press after release
                // must remain usable while stale Commit rejection stays armed.
                self.cancelled_enter_claimed = true;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner(session: usize) -> Owner {
        Owner {
            session,
            instance: Arc::new(()),
        }
    }
    fn preedit() -> Event {
        Event::Preedit("中文".into(), None)
    }
    fn commit() -> Event {
        Event::Commit("中文".into())
    }

    #[test]
    fn owner_changes_cancel_without_hidden_replay_even_after_return() {
        let a = owner(1);
        let b = owner(2);
        let mut state = Ownership::default();
        state.sync(Some(a.clone()));
        assert!(state.admit(&preedit()));
        assert!(state.sync(Some(b)));
        state.sync(Some(a));
        assert!(!state.admit(&commit()));
        assert!(state.admit(&commit()), "next commit-only input recovers");
    }

    #[test]
    fn reused_session_id_and_overlay_owners_cannot_inherit_composition() {
        for replacement in [None, Some(owner(7))] {
            let mut state = Ownership::default();
            state.sync(Some(owner(7)));
            state.admit(&preedit());
            assert!(state.sync(replacement));
            state.sync(Some(owner(7)));
            assert!(!state.admit(&commit()));
        }
    }

    #[test]
    fn empty_preedit_keeps_owner_and_normal_commit_only_input_works() {
        let mut state = Ownership::default();
        state.sync(Some(owner(1)));
        assert!(state.admit(&commit()));
        assert!(state.admit(&preedit()));
        assert!(state.admit(&Event::Preedit(String::new(), None)));
        assert!(state.admit(&commit()));
    }

    #[test]
    fn cancelled_lifecycle_can_restart_with_open_or_closed_then_preedit() {
        for reopen in [false, true] {
            let mut state = Ownership::default();
            state.sync(Some(owner(1)));
            state.admit(&preedit());
            state.sync(Some(owner(2)));
            assert!(!state.admit(&Event::Closed));
            if reopen {
                assert!(state.admit(&Event::Opened));
            }
            assert!(state.admit(&preedit()));
            assert!(state.admit(&commit()));
        }
    }

    fn enter(pressed: bool, repeat: bool) -> keyboard::Event {
        let key = keyboard::Key::Named(keyboard::key::Named::Enter);
        if pressed {
            keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat,
            }
        } else {
            keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
            }
        }
    }
    #[test]
    fn candidate_enter_is_owned_after_switch_and_stops_when_cycle_closes() {
        for overlay in [false, true] {
            let mut state = Ownership::default();
            state.sync(Some(owner(1)));
            assert!(!state.claim_enter(&enter(true, false)));
            state.admit(&preedit());
            state.sync(if overlay { None } else { Some(owner(2)) });
            assert!(state.claim_enter(&enter(true, false)));
            assert!(
                !state.claim_enter(&enter(true, true)),
                "native physical owner handles repeats"
            );
            assert!(!state.admit(&commit()));
            state.sync(Some(owner(2)));
            assert!(!state.claim_enter(&enter(true, false)));
            assert!(state.admit(&commit()));
        }
    }

    #[test]
    fn closed_cancelled_cycle_does_not_own_normal_enter_or_commit_twice() {
        let mut state = Ownership::default();
        state.sync(Some(owner(1)));
        state.admit(&preedit());
        state.sync(Some(owner(2)));
        state.admit(&Event::Closed);
        assert!(!state.claim_enter(&enter(true, false)));
        assert!(!state.admit(&commit()));
        assert!(state.admit(&commit()));
    }
    #[test]
    fn cancelled_cycle_claims_only_one_enter_but_still_rejects_stale_commit() {
        let mut state = Ownership::default();
        state.sync(Some(owner(1)));
        state.admit(&preedit());
        state.sync(Some(owner(2)));
        assert!(state.claim_enter(&enter(true, false)));
        assert!(!state.claim_enter(&enter(false, false)));
        assert!(!state.claim_enter(&enter(true, false)));
        assert!(!state.claim_enter(&enter(true, false)));
        assert!(!state.admit(&commit()));
    }
}
