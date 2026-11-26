//! Frozen native synchronized-output watchdog/end bodies.
use super::*;
use jterm_engine::synchronized_output::SyncEndEffect;
use std::cell::RefCell;
impl TerminalState {
    fn frozen_sync_mode(&mut self, enabled: bool) {
        if enabled {
            self.vt.protocol.modes_mut().insert(2026);
            self.vt.sync_output.active = true;
            self.vt.sync_output
                .started
                .get_or_insert_with(std::time::Instant::now);
        } else {
            self.frozen_sync_finish();
        }
    }

    fn frozen_sync_timeout(&mut self, now: std::time::Instant) {
        if self.vt.sync_output.started.is_some_and(|start| {
            now.saturating_duration_since(start) >= std::time::Duration::from_secs(1)
        }) {
            self.frozen_sync_finish();
        }
    }
    fn frozen_sync_finish(&mut self) {
        observe(self, SyncEndEffect::Archive);
        if self.vt.screen.use_alt_buffer {
            self.archive_alt_screen_frame();
        } else {
            self.last_synced_primary_screen_snapshot =
                self.visible_screen_snapshot().unwrap_or_default();
        }
        self.vt.protocol.modes_mut().remove(&2026);
        self.vt.sync_output.active = false;
        self.vt.sync_output.started = None;
        observe(self, SyncEndEffect::Dirty);
        self.mark_rows_dirty(0, self.vt.screen.grid.rows().saturating_sub(1));
    }
}

type Snapshot = std::collections::BTreeMap<String, serde_json::Value>;
thread_local! { static TRACE: RefCell<Option<Vec<(SyncEndEffect,Snapshot)>>> = const { RefCell::new(None) }; }
fn snapshot(t: &mut TerminalState) -> Snapshot {
    let mut s = parser_oracle_tests::snapshot(t);
    s.insert(
        "sync-start-present".into(),
        serde_json::json!(t.vt.sync_output.started.is_some()),
    );
    s
}
pub(super) fn observe(t: &mut TerminalState, effect: SyncEndEffect) {
    if TRACE.with(|v| v.borrow().is_none()) {
        return;
    }
    let s = snapshot(t);
    TRACE.with(|v| v.borrow_mut().as_mut().unwrap().push((effect, s)));
}
fn capture(f: impl FnOnce()) -> Vec<(SyncEndEffect, Snapshot)> {
    TRACE.with(|v| *v.borrow_mut() = Some(vec![]));
    f();
    TRACE.with(|v| v.borrow_mut().take().unwrap())
}
#[test]
fn sync_owner_matches_frozen_frost_boundaries_and_callback_state() {
    use jterm_engine::mode_mutation::NativeModeEffect;
    for alt in [false, true] {
        for inline in [false, true] {
            let make = || {
                let mut t = TerminalState::new(7, 3);
                t.process_input(b"old\r\nhistory\r\nmore\r\nlast");
                if alt {
                    t.enter_alternate_screen(1049);
                }
                if inline {
                    t.set_inline_cli_launch(true);
                }
                t
            };
            let mut a = make();
            let mut e = make();
            for enabled in [false, true, true, false, true] {
                let first = a.vt.sync_output.started;
                let x = capture(|| {
                    a.apply_mode_effect(NativeModeEffect::SynchronizedOutput { enabled })
                });
                let y = capture(|| e.frozen_sync_mode(enabled));
                assert_eq!(x, y);
                assert_eq!(snapshot(&mut a), snapshot(&mut e));
                if enabled && first.is_some() {
                    assert_eq!(a.vt.sync_output.started, first);
                }
            }
            let a_start = a.vt.sync_output.started.unwrap();
            let e_start = e.vt.sync_output.started.unwrap();
            for age in [
                std::time::Duration::from_millis(999),
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(1) + std::time::Duration::from_nanos(1),
            ] {
                let x = capture(|| a.check_sync_output_timeout(a_start + age));
                let y = capture(|| e.frozen_sync_timeout(e_start + age));
                assert_eq!(x, y);
                assert_eq!(snapshot(&mut a), snapshot(&mut e));
            }
        }
    }
}
