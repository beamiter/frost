
// Appended ONLY to the copied Winit X11 source in the isolated acceptance fixture.
// Record at most 64 raw pointer observations after admission proof is published.
// No application input, latch, focus, or event-filter result is changed.
fn frost_fixture_x11_pointer(stage: &str, detail: &str) {
    let Some(proof) = std::env::var_os("FROST_OVERLOAD_FIXTURE_PROOF") else { return; };
    if !std::path::Path::new(&proof).exists() { return; }
    static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 64 {
        eprintln!("FROST_FAULT_X11 pid={} stage={} {}", std::process::id(), stage, detail);
    }
}
