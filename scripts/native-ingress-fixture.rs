// Appended ONLY to the isolated copied iced_winit crate by the fixture builder.
// It stalls consumer polling, never the native compositor callback dispatcher.
static FIXTURE_NATIVE_REJECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn frost_fixture_pause(is_keyboard: bool) -> bool {
    use std::sync::{Mutex, OnceLock, atomic::{AtomicBool, Ordering}};
    static STATE: OnceLock<Mutex<Option<(std::time::Instant, usize)>>> = OnceLock::new();
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::SeqCst) {
        if is_keyboard {
            static FAULT_KEYS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let count = FAULT_KEYS.fetch_add(1, Ordering::SeqCst) + 1;
            if count % 128 == 0 {
                let path = std::path::PathBuf::from(std::env::var_os("FROST_OVERLOAD_FIXTURE_PROOF").unwrap()).with_extension("fault-keys");
                let tmp = path.with_extension(format!("fault-keys.{}.tmp", std::process::id()));
                std::fs::write(&tmp, format!("{} {}", std::process::id(), count)).unwrap();
                std::fs::rename(tmp, path).unwrap();
            }
        }
        return false;
    }
    let proof = std::path::PathBuf::from(std::env::var_os("FROST_OVERLOAD_FIXTURE_PROOF").expect("fixture proof path"));
    let state = STATE.get_or_init(|| {
        std::fs::write(proof.with_extension("armed"), b"actual Runner fixture armed\n").unwrap();
        Mutex::new(None)
    });
    let trigger = std::path::PathBuf::from(std::env::var_os("FROST_OVERLOAD_FIXTURE_CONTROL").expect("fixture control path"));
    if !trigger.exists() { return false; }
    let mut state = state.lock().unwrap();
    let (start, count) = state.get_or_insert_with(|| (std::time::Instant::now(), 0));
    if is_keyboard { *count += 1; }
    if subscription::Event::input_overloaded() {
        assert!(FIXTURE_NATIVE_REJECTED.load(Ordering::SeqCst), "fault did not originate in actual Runner admission");
        let json = format!("{{\"mechanism\":\"actual Runner admission with controlled consumer stall and real compositor keys\",\"events_broadcast\":{},\"pid\":{},\"input_overloaded\":true,\"runner_admission_rejected\":true,\"natural_winit_flood\":false,\"direct_latch_set_or_reset\":false,\"consumer_stall_ms\":{}}}", *count, std::process::id(), start.elapsed().as_millis());
        let tmp = proof.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, json).unwrap();
        std::fs::rename(tmp, proof).unwrap();
        DONE.store(true, Ordering::SeqCst);
        return false;
    }
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "actual ingress did not overload during bounded consumer stall");
    true
}

// Diagnostic proof of actual consumer progress, never an overload trigger.
fn frost_fixture_milestone(label: &str, bit: u32) {
    if !subscription::Event::input_overloaded() { return; }
    static SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if SEEN.fetch_or(1 << bit, std::sync::atomic::Ordering::SeqCst) & (1 << bit) != 0 { return; }
    let path = std::path::PathBuf::from(std::env::var_os("FROST_OVERLOAD_FIXTURE_PROOF").unwrap()).with_extension(label);
    let tmp = path.with_extension(format!("{label}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, std::process::id().to_string()).unwrap();
    std::fs::rename(tmp, path).unwrap();
}

// At most 64 small synthetic-fixture observations; no keyboard text is logged.
fn frost_fixture_trace(stage: &str, detail: &str) {
    if !subscription::Event::input_overloaded() { return; }
    static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let index = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if index < 64 {
        eprintln!("FROST_FAULT_POINTER pid={} stage={} {}", std::process::id(), stage, detail);
    }
}
fn frost_fixture_pointer(stage: &str, event: &winit::event::WindowEvent) {
    use winit::event::WindowEvent;
    if matches!(event, WindowEvent::CursorMoved { .. } | WindowEvent::CursorEntered { .. }
        | WindowEvent::CursorLeft { .. } | WindowEvent::MouseInput { .. }) {
        frost_fixture_trace(stage, &format!("{:?}", event));
    }
}
