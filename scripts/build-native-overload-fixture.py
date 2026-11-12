#!/usr/bin/env python3
"""Build an isolated GUI overload-admission fixture; never rewrite production sources.

The fixture drives a separate real Tracker into its production budget rejection.
It reuses production main/view/update, but is NOT a natural winit flood test or
an unmodified release artifact. All source changes and binary hashes are saved.
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
            "input_overloaded": Event::input_overloaded(),
            "natural_winit_flood": false,
            "direct_latch_set_or_reset": false,
        });
        std::fs::write(proof_path.with_extension("tmp"), serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
        std::fs::rename(proof_path.with_extension("tmp"), &proof_path).unwrap();
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
    args = parser.parse_args()
    root, out = args.root.resolve(strict=True), args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    checkout = out / 'checkout'
    checkout.mkdir()
    source = root / 'src/main.rs'
    original = source.read_text()
    anchor = 'fn main() -> iced::Result {'
    if original.count(anchor) != 1:
        raise RuntimeError('Expected exactly one production main; refusing ambiguous injection')
    generated = original.replace(anchor, anchor + '\n    native_overload_acceptance_start();', 1) + HELPER
    for entry in root.iterdir():
        if entry.name in {'src', 'Cargo.toml', 'Cargo.lock', '.git', 'target'}:
            continue
        if entry == out or entry in out.parents:
            continue
        (checkout / entry.name).symlink_to(entry, target_is_directory=entry.is_dir())
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
    source_hashes = {str(p.relative_to(root)): sha(p) for p in sorted(paths)}
    target = Path(os.environ.get('CARGO_TARGET_DIR', str(root / 'target'))).resolve()
    evidence = {
        'scope': 'real admission rejection + production GUI; not natural winit flood or unmodified release',
        'git_sha': subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip(),
        'production_sources_sha256': source_hashes,
        'fixture_source_sha256': sha(fixture),
        'fixture_diff_sha256': sha(out / 'fixture-main.diff'),
        'fixture_manifest_sha256': sha(checkout / 'Cargo.toml'),
        'status': 'prepared',
    }
    evidence_path = out / 'manifest.json'
    evidence_path.write_text(json.dumps(evidence, indent=2) + '\n')
    if args.prepare_only:
        print(evidence_path)
        return
    env = os.environ.copy()
    env['CARGO_TARGET_DIR'] = str(target)
    command = ['cargo', 'build', '--release', '--locked', '--manifest-path', str(checkout / 'Cargo.toml'), '--bin', 'frost-native-overload-fixture']
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
    shutil.copy2(target / 'release/frost-native-overload-fixture', binary)
    evidence.update(status='built', binary_sha256=sha(binary), build_command=command)
    evidence_path.write_text(json.dumps(evidence, indent=2) + '\n')
    binary.with_suffix('.source-manifest.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print(binary)


if __name__ == '__main__':
    main()
