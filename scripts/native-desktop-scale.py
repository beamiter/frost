#!/usr/bin/env python3
"""Bounded real Wayland125%/150% DPI gate; no clipboard/Ready OCR certification.

The source-manifested app and independent production-source observer must match.
Uses existing native-desktop-smoke solely for public compositor/keyboard/PTY
input. Creates only private fixture config and immutable evidence, no install.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import time
import native_scale_geometry as geometry


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--binary-manifest', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--runtime-wrapper', type=Path)
    parser.add_argument('--deps', type=Path)
    parser.add_argument('--rustc', type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    binary, out = args.binary.resolve(strict=True), args.output.resolve()
    source = json.loads(args.binary_manifest.read_text())
    binary_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
    geometry.require(binary_sha == source['binary_sha256'], 'Actual app SHA differs from frozen source manifest')
    geometry.require(source.get('source_unchanged_during_build') is True, 'Source was not frozen during app build')
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    runner = Path(__file__).with_name('native-desktop-smoke.py')
    spec = importlib.util.spec_from_file_location('native_scale_smoke', runner)
    module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    smoke = module.Smoke(binary, 'wayland', out)
    result = {'status': 'running', 'scope': 'DPI-only exact production geometry gate; not clipboard or recovery Ready OCR suite',
              'binary_sha256': binary_sha, 'source_manifest': source,
              'runner_sha256': hashlib.sha256(runner.read_bytes()).hexdigest(),
              'scales': list(geometry.SCALES), 'backend': 'Wayland',
              'environment': {'ICED_BACKEND': smoke.env.get('ICED_BACKEND'),
                              'WGPU_BACKEND': smoke.env.get('WGPU_BACKEND'),
                              'VK_ICD_FILENAMES': smoke.env.get('VK_ICD_FILENAMES')}}
    try:
        probe = geometry.build_probe(out / 'production-geometry', args.deps, args.rustc)
        probe_source = json.loads((probe.parent / 'source-manifest.json').read_text())
        geometry.require(probe_source['rust_sources_sha256'] == source['rust_sources_sha256'],
                         'Independent observer source differs from frozen application source')
        for key in ('cargo_toml_sha256', 'cargo_lock_sha256'):
            geometry.require(probe_source[key] == source[key], 'Observer/app dependency contract mismatch: ' + key)
        core = geometry.ROOT.parent / 'jterm_core'
        core_source = subprocess.check_output(['git', '-C', str(core), 'show',
                                               source['shared_core_commit'] + ':src/reference_shell.rs'])
        geometry.require(hashlib.sha256(core_source).hexdigest() ==
                         probe_source['production_sources_sha256'][str((core / 'src/reference_shell.rs').resolve())],
                         'Observer shared shell differs from app-manifest core source')
        result['probe_source_manifest'] = probe_source
        smoke.start_desktop()
        smoke.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'persistent-test-keyboard')
        smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "block"\nrestore_session = false\n'
            'font_family = "DejaVu Sans Mono"\nfont_size = 14\nchrome_font_size = 14\nline_spacing = 1\npadding = 2\n'
            'ui_scale = 1\ninitial_width = 1100\ninitial_height = 760\nascii_organism_enabled = false\njsh_update_check = "never"\n')
        config_before = smoke.config.read_bytes()
        result['synthetic_config_sha256'] = hashlib.sha256(config_before).hexdigest()
        command = ([str(args.runtime_wrapper.resolve(strict=True))] if args.runtime_wrapper else []) + [str(binary)]
        app = smoke.spawn(command, 'frost-dpi')
        module.wait_for(smoke.app_window, 'owned Frost DPI window')
        time.sleep(1)
        original = smoke.command('dpi-ready')
        def record(label):
            path = out / (label + '-stty.txt')
            smoke.text('stty size > ' + shlex.quote(str(path)))
            smoke.key('Return')
            identity = smoke.command(label)
            geometry.require(identity == original, 'Native DPI input replaced original shell')
            rows, cols = map(int, path.read_text().split())
            pid, tty = identity.split(':', 1)
            return {'pid': int(pid), 'tty': tty, 'rows': rows, 'cols': cols}
        result['records'] = geometry.run_sequence(smoke, app.pid, probe, record)
        smoke.run(['swaymsg', '[app_id="io.github.beamiter.frost"]', 'kill'])
        app.wait(timeout=20)
        geometry.require(app.returncode == 0, 'DPI app did not close gracefully')
        geometry.require(hashlib.sha256(binary.read_bytes()).hexdigest() == binary_sha,
                         'Frozen app binary changed during native DPI run')
        geometry.require(smoke.config.read_bytes() == config_before, 'DPI input changed the configured font/UI contract')
        result['status'] = 'dpi-only-pass'
    except BaseException as error:
        result.update(status='failed', error=repr(error))
        try: smoke.screenshot('dpi-failure')
        except Exception: pass
        raise
    finally:
        try: smoke.close()
        except BaseException as error:
            result.update(status='failed', cleanup_error=repr(error))
            raise
        finally: (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__': main()
