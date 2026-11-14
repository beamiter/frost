//! Opt-in, content-free close tracing for native acceptance failures.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    OnceLock,
};

const LIMIT: usize = 128;
struct Recorder {
    enabled: bool,
    armed: AtomicBool,
    count: AtomicUsize,
}
impl Recorder {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            armed: AtomicBool::new(false),
            count: AtomicUsize::new(0),
        }
    }
    fn slot(&self, begin: bool) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        if begin {
            self.armed.store(true, Ordering::Relaxed);
        }
        if !self.armed.load(Ordering::Relaxed) {
            return None;
        }
        let mut count = self.count.load(Ordering::Relaxed);
        while count < LIMIT {
            match self.count.compare_exchange_weak(
                count,
                count + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(count),
                Err(current) => count = current,
            }
        }
        None
    }
}

/// Closure formatting is skipped entirely when disabled or the budget is spent.
/// Callers supply only fixed labels, revisions, and booleans; never user content.
pub(crate) fn record(begin: bool, detail: impl FnOnce() -> String) {
    static RECORDER: OnceLock<Recorder> = OnceLock::new();
    let recorder = RECORDER.get_or_init(|| {
        Recorder::new(std::env::var("FROST_CLOSE_DIAGNOSTICS").as_deref() == Ok("1"))
    });
    let Some(slot) = recorder.slot(begin) else {
        return;
    };
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC is shared with Python's monotonic_ns on the native Linux runner.
    let clock_ok = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } == 0;
    let ns = i128::from(ts.tv_sec) * 1_000_000_000 + i128::from(ts.tv_nsec);
    let detail = if slot == LIMIT - 1 {
        "budget_exhausted".to_owned()
    } else {
        detail()
    };
    // Diagnostics must not panic if a caller has closed stderr.
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "[frost-close] seq={slot} monotonic_ns={ns} clock_ok={clock_ok} {detail}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_and_unarmed_do_not_consume_budget() {
        let disabled = Recorder::new(false);
        assert_eq!(disabled.slot(true), None);
        assert_eq!(disabled.count.load(Ordering::Relaxed), 0);
        let enabled = Recorder::new(true);
        assert_eq!(enabled.slot(false), None);
        assert_eq!(enabled.slot(true), Some(0));
        assert_eq!(enabled.slot(false), Some(1));
    }
    #[test]
    fn concurrent_recording_has_a_lifetime_bound() {
        let recorder = Recorder::new(true);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..256 {
                        recorder.slot(true);
                    }
                });
            }
        });
        assert_eq!(recorder.count.load(Ordering::Relaxed), LIMIT);
        assert_eq!(recorder.slot(true), None);
    }
}
