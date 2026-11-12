//! Retained native adapter ownership; not an OS or whole-process memory limit.
use std::sync::{Arc, Mutex};

const MAX_EVENTS: usize = 4096;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Default)]
struct Usage {
    events: usize,
    bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Budget(Arc<Mutex<Usage>>, usize, usize);
impl Default for Budget {
    fn default() -> Self {
        Self::with_limits(MAX_EVENTS, MAX_BYTES)
    }
}

#[derive(Debug)]
pub(crate) struct Lease {
    budget: Budget,
    bytes: usize,
}

impl Budget {
    pub(crate) fn with_limits(events: usize, bytes: usize) -> Self {
        Self(Arc::new(Mutex::new(Usage::default())), events, bytes)
    }
    /// Never waits for the GUI consumer to release capacity.
    pub(crate) fn admit(&self, bytes: usize) -> Option<Lease> {
        let mut usage = self.0.lock().expect("native event budget poisoned");
        let next = usage.bytes.checked_add(bytes)?;
        if usage.events >= self.1 || next > self.2 {
            return None;
        }
        usage.events += 1;
        usage.bytes = next;
        Some(Lease {
            budget: self.clone(),
            bytes,
        })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut usage = self.budget.0.lock().expect("native event budget poisoned");
        usage.events -= 1;
        usage.bytes -= self.bytes;
    }
}

/// Charge owned dynamic native payloads before this adapter retains them.
/// Small-string key payloads use their logical length (their inline storage is
/// already part of WindowEvent); IME uses actual String allocation capacity.
pub(crate) fn window_event_bytes(event: &mut winit::event::WindowEvent) -> usize {
    use winit::{
        event::{Ime, WindowEvent},
        keyboard::Key,
    };
    let dynamic = match event {
        WindowEvent::Ime(Ime::Preedit(s, _) | Ime::Commit(s)) => s.capacity(),
        WindowEvent::ActivationTokenDone { token, .. } => activation_token_bytes(token),
        WindowEvent::DroppedFile(path) | WindowEvent::HoveredFile(path) => path.capacity(),
        WindowEvent::KeyboardInput { event, .. } => {
            let key = match &event.logical_key {
                Key::Character(s) => s.len(),
                _ => 0,
            };
            let mut bytes = key.saturating_add(event.text.as_ref().map_or(0, |s| s.len()));
            #[cfg(not(target_arch = "wasm32"))]
            {
                use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
                bytes = bytes.saturating_add(event.text_with_all_modifiers().map_or(0, str::len));
                if let Key::Character(s) = event.key_without_modifiers() {
                    bytes = bytes.saturating_add(s.len());
                }
            }
            bytes
        }
        _ => 0,
    };
    std::mem::size_of::<winit::event::WindowEvent>().saturating_add(dynamic)
}

// Winit exposes ownership conversion, not a borrowed capacity accessor. Move
// the String out and back without cloning, logging, or changing its allocation.
fn activation_token_bytes(token: &mut winit::window::ActivationToken) -> usize {
    let raw = std::mem::replace(
        token,
        winit::window::ActivationToken::from_raw(String::new()),
    )
    .into_raw();
    let bytes = raw.capacity();
    *token = winit::window::ActivationToken::from_raw(raw);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn data_saturation_cannot_consume_fault_control_reservations() {
        let normal = Budget::default();
        let _full = normal.admit(MAX_BYTES).unwrap();
        let buttons = Budget::with_limits(32, 64 * 1024);
        let pointer = Budget::with_limits(128, 128 * 1024);
        let paint = Budget::with_limits(64, 128 * 1024);
        let _moves: Vec<_> = (0..128).map(|_| pointer.admit(1).unwrap()).collect();
        assert!(normal.admit(1).is_none());
        assert!(pointer.admit(1).is_none());
        assert!(buttons.admit(1).is_some());
        assert!(paint.admit(1).is_some());
    }
    #[test]
    fn copies_share_capacity_until_batch_lease_dropped() {
        let budget = Budget::default();
        let clone = budget.clone();
        let held: Vec<_> = (0..MAX_EVENTS).map(|_| budget.admit(1).unwrap()).collect();
        assert!(clone.admit(1).is_none());
        drop(held);
        assert!(clone.admit(MAX_BYTES).is_some());
    }
    #[test]
    fn byte_limit_overflow_and_cancellation_conserve_usage() {
        let budget = Budget::default();
        assert!(budget.admit(usize::MAX).is_none());
        let lease = budget.admit(MAX_BYTES).unwrap();
        assert!(budget.admit(1).is_none());
        drop(lease);
        assert!(budget.admit(MAX_BYTES).is_some());
        assert_eq!(budget.0.lock().unwrap().bytes, 0);
    }
    #[test]
    fn activation_token_retained_capacity_is_measured_without_copying() {
        let mut raw = String::with_capacity(MAX_BYTES + 1);
        raw.push('x');
        let ptr = raw.as_ptr();
        let capacity = raw.capacity();
        let mut token = winit::window::ActivationToken::from_raw(raw);
        assert_eq!(activation_token_bytes(&mut token), capacity);
        let raw = token.into_raw();
        assert_eq!(raw.as_ptr(), ptr);
        assert_eq!(raw.capacity(), capacity);
        assert_eq!(raw, "x");
    }
    #[test]
    fn ime_accounts_retained_capacity_not_only_length() {
        let mut s = String::with_capacity(MAX_BYTES);
        s.push('x');
        let mut event = winit::event::WindowEvent::Ime(winit::event::Ime::Commit(s));
        assert!(window_event_bytes(&mut event) > MAX_BYTES);
        assert!(
            Budget::default()
                .admit(window_event_bytes(&mut event))
                .is_none()
        );
    }
}

/// One retained lifecycle wake per kind while the consumer is suspended.
/// Only idempotent lifecycle notifications use this, never user input.
#[derive(Debug, Clone, Default)]
pub(crate) struct WakeFlag(Arc<std::sync::atomic::AtomicBool>);
#[derive(Debug)]
pub(crate) struct WakeLease(WakeFlag);
impl WakeFlag {
    pub(crate) fn claim(&self) -> Option<WakeLease> {
        (!self.0.swap(true, std::sync::atomic::Ordering::AcqRel)).then(|| WakeLease(self.clone()))
    }
}
impl Drop for WakeLease {
    fn drop(&mut self) {
        self.0.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;
    #[test]
    fn lifecycle_is_coalesced_only_while_pending_and_cancel_rearms() {
        let flag = WakeFlag::default();
        let held = flag.claim().unwrap();
        for _ in 0..10_000 {
            assert!(flag.claim().is_none());
        }
        drop(held);
        assert!(flag.claim().is_some());
        assert!(flag.claim().is_some());
    }
}

/// Admit only ready actions; Pending is handled by the existing async runtime.
/// On rejection, return ownership so the caller can latch BEFORE dropping it.
pub(crate) fn admit_ready_action<T>(actions: &mut Vec<T>, action: T) -> Result<(), T> {
    if actions.len() >= 256 {
        Err(action)
    } else {
        actions.push(action);
        Ok(())
    }
}
#[cfg(test)]
mod ready_tests {
    use super::*;
    #[test]
    fn exact_cap_is_admitted_fifo_and_cap_plus_one_is_returned_not_reordered() {
        let mut actions = Vec::new();
        for i in 0..256 {
            assert!(admit_ready_action(&mut actions, i).is_ok());
        }
        assert_eq!(actions, (0..256).collect::<Vec<_>>());
        assert_eq!(admit_ready_action(&mut actions, 256), Err(256));
        assert_eq!(actions.len(), 256);
        // The caller rejects this not-yet-applied batch only after fault latching.
        actions.clear();
        assert!(actions.is_empty());
    }
}
