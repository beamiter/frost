#!/usr/bin/env python3
"""Derived-fixture GUI acceptance, NOT natural winit end-to-end flood coverage.

The isolated fixture exercises real Tracker admission. Production view/update,
actual compositor input, screenshots, shell effects and normal exit are observed.
No production trigger/reset API or synthetic application Message is used.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import time
import unittest


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


HERE = Path(__file__).resolve().parent
runner = module('overload_desktop_runner', HERE / 'native-desktop-smoke.py')
ocr = module('overload_ocr', HERE / 'native-recovery-report.py')
SCOPE = 'derived fixture: real Tracker admission plus production fault GUI; not natural winit end-to-end flood'
BUTTON = 'Close Frost and stop jobs'


def fault_button(words):
    for phrase in ('Input stopped:', 'Pending input was rejected and will not be replayed.',
                   'may terminate running terminal jobs', 'lose unsaved editor changes.'):
        if not ocr.exact_phrase(words, phrase):
            return None
    matches = ocr.exact_phrase(words, BUTTON)
    if len(matches) != 1:
        return None
    match = matches[0]
    if any(word['confidence'] < 50 for word in match):
        return None
    left = min(word['left'] for word in match)
    right = max(word['left'] + word['width'] for word in match)
    top = min(word['top'] for word in match)
    bottom = max(word['top'] + word['height'] for word in match)
    if not (0 <= left < right <= 1280 and 0 <= top < bottom <= 900):
        return None
    return [(left + right) // 2, (top + bottom) // 2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--backend', choices=['x11', 'wayland'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    manifest = json.loads(args.manifest.read_text())
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    assert manifest['binary_sha256'] == digest, 'Fixture binary differs from build evidence'
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    (out / 'fixture-manifest.json').write_text(json.dumps(manifest, indent=2))
    result = {'scope': SCOPE, 'backend': args.backend, 'binary_sha256': digest, 'status': 'fail'}
    smoke = runner.Smoke(binary, args.backend, out)
    try:
        smoke.start_desktop()
        if args.backend == 'wayland':
            smoke.spawn(['wtype', '-s', '600000', '-k', 'F24'], 'seat-keyboard')
        control, proof = out / 'trigger', out / 'admission.json'
        smoke.env.update(FROST_OVERLOAD_FIXTURE_CONTROL=str(control),
                         FROST_OVERLOAD_FIXTURE_PROOF=str(proof))
        smoke.config.write_text('shell = "/bin/bash"\nterminal_mode = "normal"\n'
                                'initial_width = 1100\ninitial_height = 760\n'
                                'restore_session = false\neva_companion_enabled = false\njsh_update_check = \"never\"\n')
        app = smoke.spawn([str(binary)], 'fixture')
        runner.wait_for(smoke.app_window, 'real Frost window')
        smoke.select_terminal(require_terminal=True)
        result['baseline_pty_identity'] = smoke.command('overload-before')
        smoke.screenshot('before-overload')
        runner.wait_for(proof.with_suffix('.armed').exists, 'fixture Tracker armed', timeout=10)
        control.write_text('trigger real admission\n')
        runner.wait_for(proof.exists, 'real admission proof', timeout=10)
        admission = json.loads(proof.read_text())
        assert admission['input_overloaded'] is True, admission
        assert 0 < admission['events_broadcast'] <= 65536, admission
        assert admission['natural_winit_flood'] is False, admission
        assert admission['direct_latch_set_or_reset'] is False, admission
        result['admission'] = admission
        observations = []

        def observe_fault():
            assert app.poll() is None, 'Application exited before fault UI observation'
            name = 'fault-' + str(len(observations))
            smoke.screenshot(name)
            words, transform = ocr.capture_ocr(out / (name + '.png'), smoke.run)
            point = fault_button(words)
            observations.append({'frame': name, 'transform': transform, 'button': point})
            return point

        point = runner.wait_for(observe_fault, 'visible overload and explicit exit warning', timeout=20)
        marker = out / 'forbidden-after-overload'
        smoke.text('printf SHOULD_NOT_RUN > ' + shlex.quote(str(marker)))
        smoke.key('Return')
        time.sleep(0.5)
        assert not marker.exists(), 'Post-overload native input reached the shell'
        # Re-observe after native typing so stale coordinates cannot authorize a click.
        point = runner.wait_for(observe_fault, 'fault remains latched after input', timeout=10)
        result['observations'] = observations
        result['click'] = {'text': BUTTON, 'point': point, 'method': 'native compositor pointer'}
        if args.backend == 'x11':
            smoke.run(['xdotool', 'mousemove', '--sync', *map(str, point), 'click', '1'])
        else:
            pointer = smoke.env.get('FROST_SMOKE_POINTER')
            assert pointer, 'Missing genuine Wayland pointer helper'
            log = open(out / 'pointer.log', 'w')
            smoke.logs.append(log)
            proc = subprocess.Popen([pointer], env=smoke.env, stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=log, bufsize=0)
            smoke.processes.append(proc)
            smoke.pointer_reply(proc, 'ready')
            proc.stdin.write(f'click {point[0]} {point[1]} 1280 900\n'.encode())
            proc.stdin.flush()
            smoke.pointer_reply(proc, 'done')
        # Cleanup kills never count as success. Require the actual button to exit.
        result['exit_code'] = app.wait(timeout=20)
        assert result['exit_code'] == 0, result
        assert not marker.exists(), 'Quarantined input was replayed during close'
        result.update(status='pass', forbidden_marker_absent=True)
    except Exception as error:
        result['error'] = str(error)
        try:
            smoke.screenshot('failure')
        except Exception:
            pass
        raise
    finally:
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        smoke.close()


class ObservationTests(unittest.TestCase):
    def words(self, button=BUTTON):
        lines = ['Input stopped:', 'Pending input was rejected and will not be replayed.',
                 'may terminate running terminal jobs', 'lose unsaved editor changes.', button]
        return [dict(text=word, confidence=90, left=10 + x * 50, top=20 + y * 40,
                     width=45, height=20, line=(1, 1, 1, y))
                for y, line in enumerate(lines) for x, word in enumerate(line.split())]

    def test_requires_complete_warning_and_exact_unique_button(self):
        self.assertIsNotNone(fault_button(self.words()))
        self.assertIsNone(fault_button(self.words('Close anyway')))
        self.assertIsNone(fault_button(self.words()[2:]))

    def test_does_not_guess_ambiguous_or_low_confidence_button(self):
        words = self.words()
        words[-1]['confidence'] = 10
        self.assertIsNone(fault_button(words))
        words = self.words()
        words.extend(dict(word, line=(2, 2, 2, 2), top=800) for word in self.words()[-5:])
        self.assertIsNone(fault_button(words))


if __name__ == '__main__':
    import sys
    if sys.argv[1:] == ['--self-test']:
        unittest.main(argv=[sys.argv[0]])
    else:
        main()
