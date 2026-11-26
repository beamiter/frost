//! Frozen pre-extraction native h/l dispatch and application mode callbacks.
//! Neither reference path calls NativeModeCommand or its parameter mutation.
use super::*;
use jterm_engine::mode_mutation::{apply_frost_mode, FrostModeEffect, ModeNamespace};
use std::cell::RefCell;
use std::collections::BTreeMap;
type Snapshot = BTreeMap<String, serde_json::Value>;
struct TraceState {
    rows: Vec<Snapshot>,
    guard: crate::relative_scroll_admission::Identity,
    guard_changes: usize,
    sync: Option<std::time::Instant>,
    sync_changes: usize,
}
thread_local! { static TRACE: RefCell<Option<TraceState>> = const { RefCell::new(None) }; }

pub(super) fn observe(t: &mut TerminalState) {
    TRACE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            let guard = t.projection_scroll_guard.snapshot();
            if guard != trace.guard {
                trace.guard_changes += 1;
                trace.guard = guard;
            }
            if t.vt.sync_output.started != trace.sync {
                trace.sync_changes += 1;
                trace.sync = t.vt.sync_output.started;
            }
            let mut row = snapshot(t);
            row.insert(
                "guard_changes".into(),
                serde_json::json!(trace.guard_changes),
            );
            row.insert("sync_changes".into(), serde_json::json!(trace.sync_changes));
            trace.rows.push(row);
        }
    });
}
fn snapshot(t: &mut TerminalState) -> Snapshot {
    let mut row = parser_oracle_tests::snapshot(t);
    row.insert(
        "sync_start_present".into(),
        serde_json::json!(t.vt.sync_output.started.is_some()),
    );
    row
}
fn seeded(seed: usize) -> TerminalState {
    let mut t = TerminalState::new(6, 5);
    t.process_input(b"old\r\nretained\r\nmore\r\nrows\r\nlast");
    if seed & 1 != 0 {
        t.process_input(b"\x1b[?1049h");
    }
    if seed & 2 != 0 {
        t.process_input(b"\x1b[?2026h");
    }
    t.process_input(b"\x1b[?1;66;1000;1004;2004h\x1b[?7;25l\x1b[4h\x1b[2;4r\x1b[2;3H\x1b7");
    t.vt.screen.current_bg = Color::Indexed(201);
    t.vt.screen.pending_wrap = true;
    t
}
impl TerminalState {
    fn frozen_command_apply_mode(&mut self, namespace: ModeNamespace, mode: u16, enabled: bool) {
        let effect = apply_frost_mode(
            &mut self.vt.screen,
            &mut self.vt.protocol,
            namespace,
            mode,
            enabled,
        );
        // Apply host effects before the next parameter, even when a single
        // input batch toggles the mode back to its starting state.
        match effect {
            FrostModeEffect::None => {}
            FrostModeEffect::MouseReportingChanged => self.projection_scroll_guard.revoke(),
            FrostModeEffect::AlternateScreen => {
                if enabled {
                    self.enter_alternate_screen(mode);
                } else {
                    self.leave_alternate_screen(mode);
                }
            }
            FrostModeEffect::SynchronizedOutput => {
                if enabled {
                    self.vt.protocol.modes_mut().insert(2026);
                    self.vt.sync_output.active = true;
                    self.vt.sync_output.started
                        .get_or_insert_with(std::time::Instant::now);
                } else {
                    self.finish_synchronized_output();
                }
            }
        }
    }

    fn frozen_mode_command(&mut self, params: &[u16], cmd: char, private_prefix: Option<u8>) {
        match cmd {
            'h' | 'l' => {
                let namespace = if private_prefix == Some(b'?') {
                    ModeNamespace::DecPrivate
                } else {
                    ModeNamespace::Ansi
                };
                for &mode in params {
                    self.frozen_command_apply_mode(namespace, mode, cmd == 'h');
                    observe(self);
                }
            }
            _ => {}
        }
    }
}
fn run(
    mut t: TerminalState,
    params: &[u16],
    cmd: char,
    prefix: Option<u8>,
    intermediate: &[u8],
    original: bool,
) -> (Snapshot, Vec<Snapshot>) {
    TRACE.with(|slot| {
        *slot.borrow_mut() = Some(TraceState {
            rows: Vec::new(),
            guard: t.projection_scroll_guard.snapshot(),
            guard_changes: 0,
            sync: t.vt.sync_output.started,
            sync_changes: 0,
        })
    });
    if original {
        t.frozen_mode_command(params, cmd, prefix);
    } else {
        t.handle_escape_sequence(params, b"", cmd, prefix, intermediate);
    }
    let trace = TRACE.with(|slot| slot.borrow_mut().take().unwrap().rows);
    (snapshot(&mut t), trace)
}

#[test]
fn mode_command_matches_frozen_parameter_effect_order() {
    for seed in 0..4 {
        for cmd in ['h', 'l'] {
            for prefix in [None, Some(b'?'), Some(b'>'), Some(b'=')] {
                for intermediate in [b"".as_slice(), b"!", b" "] {
                    for params in [
                        vec![],
                        vec![4, 6, 7, 25],
                        vec![1004, 1004, 5522, 1048],
                        vec![6, 1049],
                        vec![1049, 6],
                        vec![47, 1047, 1049, 1048, 6, 7],
                        vec![2026, 2026, 1049, 2026],
                        vec![5522, 1004, 2026, 0, 65535],
                        vec![1000, 1002, 1000, 1006, 1015],
                    ] {
                        assert_eq!(run(seeded(seed), &params, cmd, prefix, intermediate, false), run(seeded(seed), &params, cmd, prefix, intermediate, true), "seed={seed} cmd={cmd} prefix={prefix:?} intermediate={intermediate:?} params={params:?}");
                    }
                }
            }
        }
    }
}
