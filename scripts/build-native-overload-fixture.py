#!/usr/bin/env python3
"""Build an isolated GUI overload-admission fixture; never rewrite production sources.

The fixture drives either a separate real Tracker or actual Runner admission
under a controlled consumer stall. It reuses production main/view/update but is
NOT a natural-load flood test or an unmodified release artifact. All source changes and binary hashes are saved.
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

HELPER = r'''
// Acceptance fixture only: real admission, no latch setter/reset or host bypass.
fn native_overload_acceptance_start() {
    let trigger = std::path::PathBuf::from(std::env::var_os("FROST_OVERLOAD_FIXTURE_CONTROL")
        .expect("fixture requires FROST_OVERLOAD_FIXTURE_CONTROL"));
    let proof_path = std::path::PathBuf::from(std::env::var_os("FROST_OVERLOAD_FIXTURE_PROOF")
        .expect("fixture requires FROST_OVERLOAD_FIXTURE_PROOF"));
    assert!(trigger.parent().is_some_and(|p| p.is_dir()), "fixture control parent must exist");
    assert!(proof_path.parent().is_some_and(|p| p.is_dir()), "fixture proof parent must exist");
    std::thread::spawn(move || {
        use iced_winit::futures::subscription::{self, Event, Tracker};
        let mut tracker = Tracker::new();
        let (output, _received) = iced::futures::channel::mpsc::channel::<Event>(1);
        let subscription = subscription::filter_map("native-overload-gui-fixture", Some);
        // Keep the real recipe/input alive but deliberately do not poll it.
        let _unpolled = tracker.update(subscription::into_recipes(subscription).into_iter(), output);
        assert!(!Event::input_overloaded());
        std::fs::write(proof_path.with_extension("armed"), b"real Tracker ready\n").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !trigger.exists() {
            assert!(std::time::Instant::now() < deadline, "fixture trigger timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut count = 0;
        while !Event::input_overloaded() && count < 65536 {
            tracker.broadcast(Event::Interaction {
                window: iced::window::Id::unique(),
                event: iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                    modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                    physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Enter),
                    location: iced::keyboard::Location::Standard,
                    modifiers: iced::keyboard::Modifiers::NONE,
                    text: Some("\r".into()), repeat: false,
                }),
                status: iced::event::Status::Ignored,
            });
            count += 1;
        }
        let proof = serde_json::json!({
            "mechanism": "separate real Tracker with unpolled recipe; production admission rejection",
            "events_broadcast": count,
            "pid": std::process::id(),
            "input_overloaded": Event::input_overloaded(),
            "natural_winit_flood": false,
            "direct_latch_set_or_reset": false,
        });
        let temporary = proof_path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
        std::fs::rename(&temporary, &proof_path).unwrap();
        assert!(Event::input_overloaded(), "production budget did not reject bounded fixture flood");
    });
}
'''


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--mechanism', choices=['tracker', 'runner'], default='tracker')
    parser.add_argument('--profile', choices=['dev', 'release'], default='release')
    args = parser.parse_args()
    root, out = args.root.resolve(strict=True), args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    checkout = out / 'checkout'
    checkout.mkdir()
    source = root / 'src/main.rs'
    original = source.read_text()
    # Embedded workspace/Vim helper invocations must never start the GUI
    # fixture worker or race the parent process's proof publication.
    anchor = '    let config::ConfigLoad {'
    if original.count(anchor) != 1:
        raise RuntimeError('Expected one post-dispatch GUI config load; refusing ambiguous injection')
    generated = original.replace(anchor, '    native_overload_acceptance_start();\n' + anchor, 1) + HELPER
    if args.mechanism == 'runner':
        generated = original
    for entry in root.iterdir():
        if entry.name in {'src', 'Cargo.toml', 'Cargo.lock', '.git', 'target'}:
            continue
        if entry.name == 'vendor' and args.mechanism == 'runner':
            continue
        if entry == out or entry in out.parents:
            continue
        (checkout / entry.name).symlink_to(entry, target_is_directory=entry.is_dir())
    if args.mechanism == 'runner':
        vendor = checkout / 'vendor'
        vendor.mkdir()
        for entry in (root / 'vendor').iterdir():
            if entry.name == 'iced_winit':
                shutil.copytree(entry, vendor / entry.name)
            else:
                (vendor / entry.name).symlink_to(entry, target_is_directory=entry.is_dir())
        lib = vendor / 'iced_winit/src/lib.rs'
        production_lib = lib.read_text()
        pre = '            let event = match &mut event {'
        pause = '            // Even rejected input must poll the consumer so the fault can be shown.'
        reject = '                        None => {\n                            subscription::Event::report_input_overload();'
        for anchor in (pre, pause, reject):
            if production_lib.count(anchor) != 1:
                raise RuntimeError('Ambiguous actual Runner fixture anchor: ' + anchor)
        derived = production_lib.replace(pre, '            let fixture_key = matches!(&event, Event::EventLoopAwakened(winit::event::Event::WindowEvent { event: winit::event::WindowEvent::KeyboardInput { .. }, .. }));\n' + pre, 1)
        derived = derived.replace(pause, '            if frost_fixture_pause(fixture_key) { return; }\n' + pause, 1)
        derived = derived.replace(reject, '                        None => {\n                            if !subscription::Event::input_overloaded() { FIXTURE_NATIVE_REJECTED.store(true, std::sync::atomic::Ordering::SeqCst); }\n                            subscription::Event::report_input_overload();', 1)
        # Record real consumer milestones without setting or resetting the latch.
        milestones = [
            ('        let event = match event {', 'consumer', 0),
            ('            fault_displayed = true;', 'view-rebuilt', 1),
            ('                        draw_span.finish();', 'drawn', 2),
            ('                                window.surface_recovery.recovered();', 'presented', 3),
        ]
        for anchor, label, bit in milestones:
            if derived.count(anchor) != 1:
                raise RuntimeError('Ambiguous fixture milestone anchor: ' + label)
            derived = derived.replace(anchor, f'        frost_fixture_milestone("{label}", {bit});\n' + anchor, 1)
        # Bounded derived-only diagnostics distinguish real pointer delivery,
        # UI interaction, and exit action. They never generate input/messages.
        traces = [
            (pre, '            if let Event::EventLoopAwakened(winit::event::Event::WindowEvent { event, .. }) = &event { frost_fixture_pointer("runner", event); }\n'),
            ('                        if native_events::observe(id, &window_event) {', '                        frost_fixture_pointer("consumer", &window_event);\n'),
            ('                            let (ui_state, statuses) = user_interfaces', '                            if window_events.iter().any(|event| matches!(event, core::Event::Mouse(_))) { frost_fixture_trace("ui-before", &format!("cursor={:?} mouse_events={}", window.state.cursor(), window_events.iter().filter(|event| matches!(event, core::Event::Mouse(_))).count())); }\n'),
            ('                            match ui_state {', '                            if window_events.iter().any(|event| matches!(event, core::Event::Mouse(_))) { frost_fixture_trace("ui-after", &format!("statuses={:?} status_count={} messages={}", statuses.iter().take(8).collect::<Vec<_>>(), statuses.len(), messages.len())); }\n'),
            ('        Action::Exit => {', '        // Trace the real action, never manufacture an exit.\n'),
        ]
        for anchor, trace in traces:
            if derived.count(anchor) != 1:
                raise RuntimeError('Ambiguous fixture pointer trace anchor: ' + anchor)
            derived = derived.replace(anchor, trace + anchor, 1)
        derived = derived.replace('        Action::Exit => {', '        Action::Exit => {\n            frost_fixture_trace("exit-action", "observed");', 1)
        derived += (root / 'scripts/native-ingress-fixture.rs').read_text()
        lib.write_text(derived)
        (out / 'fixture-iced-winit.rs').write_text(derived)
        (out / 'fixture-iced-winit.diff').write_text(''.join(difflib.unified_diff(
            production_lib.splitlines(keepends=True), derived.splitlines(keepends=True),
            fromfile='production/vendor/iced_winit/src/lib.rs', tofile='fixture/vendor/iced_winit/src/lib.rs')))
    (checkout / 'src').mkdir()
    for entry in (root / 'src').iterdir():
        if entry.name == 'native_overload_fixture.rs':
            raise RuntimeError('Production tree unexpectedly contains fixture source')
        (checkout / 'src' / entry.name).symlink_to(entry, target_is_directory=entry.is_dir())
    fixture = checkout / 'src/native_overload_fixture.rs'
    fixture.write_text(generated)
    (out / 'fixture-main.rs').write_text(generated)
    (out / 'fixture-main.diff').write_text(''.join(difflib.unified_diff(
        original.splitlines(keepends=True), generated.splitlines(keepends=True),
        fromfile='production/src/main.rs', tofile='fixture/src/native_overload_fixture.rs')))
    manifest = (root / 'Cargo.toml').read_text() + '\n[[bin]]\nname = "frost-native-overload-fixture"\npath = "src/native_overload_fixture.rs"\n'
    (checkout / 'Cargo.toml').write_text(manifest)
    shutil.copy2(root / 'Cargo.lock', checkout / 'Cargo.lock')
    paths = [root / 'Cargo.toml', root / 'Cargo.lock']
    paths += list((root / 'src').rglob('*.rs'))
    paths += list((root / 'vendor/iced_futures').rglob('*.rs'))
    paths += list((root / 'vendor/iced_winit').rglob('*.rs'))
    source_hashes = {str(p.relative_to(root)): sha(p) for p in sorted(paths)}
    target = Path(os.environ.get('CARGO_TARGET_DIR', str(root / 'target'))).resolve()
    evidence = {
        'mechanism': args.mechanism,
        'profile': args.profile,
        'scope': 'real admission rejection + production GUI; not natural winit flood or unmodified release',
        'git_sha': subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip(),
        'production_sources_sha256': source_hashes,
        'fixture_source_sha256': sha(fixture),
        'fixture_diff_sha256': sha(out / 'fixture-main.diff'),
        'fixture_manifest_sha256': sha(checkout / 'Cargo.toml'),
        'status': 'prepared',
    }
    if args.mechanism == 'runner':
        evidence['scope'] = 'actual Runner admission under controlled consumer stall and real compositor input; not natural flood or unmodified release'
        evidence['runner_source_sha256'] = sha(out / 'fixture-iced-winit.rs')
        evidence['runner_diff_sha256'] = sha(out / 'fixture-iced-winit.diff')
    evidence_path = out / 'manifest.json'
    evidence_path.write_text(json.dumps(evidence, indent=2) + '\n')
    if args.prepare_only:
        print(evidence_path)
        return
    env = os.environ.copy()
    env['CARGO_TARGET_DIR'] = str(target)
    command = ['cargo', 'build'] + (['--release'] if args.profile == 'release' else []) + ['--locked', '--manifest-path', str(checkout / 'Cargo.toml'), '--bin', 'frost-native-overload-fixture']
    try:
        with (out / 'build.log').open('w') as log:
            subprocess.run(command, cwd=checkout, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    except Exception:
        evidence['status'] = 'build-failed'
        evidence_path.write_text(json.dumps(evidence, indent=2) + '\n')
        raise
    finally:
        # Never upload a symlink checkout: artifact walkers may follow vendor
        # links and accidentally archive the entire source tree. The exact
        # generated source, diff, hashes and build log are already retained.
        shutil.rmtree(checkout)
    if any(sha(root / path) != digest for path, digest in source_hashes.items()):
        raise RuntimeError('Production sources changed during fixture build; evidence invalid')
    binary = out / 'native-overload-fixture'
    shutil.copy2(target / ('release' if args.profile == 'release' else 'debug') / 'frost-native-overload-fixture', binary)
    evidence.update(status='built', binary_sha256=sha(binary), build_command=command)
    evidence_path.write_text(json.dumps(evidence, indent=2) + '\n')
    binary.with_suffix('.source-manifest.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print(binary)


if __name__ == '__main__':
    main()
