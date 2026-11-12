#!/usr/bin/env python3
"""Derived-fixture GUI acceptance, NOT natural winit end-to-end flood coverage.

The isolated fixture exercises Tracker or actual stalled Runner admission. Production view/update,
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


def capture_fault_ocr(png, run):
    """Fixed color-preserving pipeline for mixed-polarity fault UI.

    White paragraphs on a dark surface and dark text on the blue action button
    must both survive. Global grayscale inversion and sparse PSM 11 can omit
    the button. Keep the entire raw color frame and use automatic page layout;
    never guess/crop a button rectangle or adapt pixels to the expected text.
    """
    assert ocr.png_extent(png) == ocr.SCREEN, 'Unexpected capture extent'
    processed = png.with_suffix('.fault-ocr.png')
    command = ['convert', str(png), '-filter', 'Lanczos', '-resize', '300%', str(processed)]
    run(command, capture_output=True)
    assert ocr.png_extent(processed) == (3840, 2700), 'Unexpected OCR transform'
    tsv = run(['tesseract', str(processed), 'stdout', '--psm', '3', 'tsv'],
              text=True, capture_output=True).stdout
    tsv_path = png.with_suffix('.fault-ocr.tsv')
    tsv_path.write_text(tsv)
    words = ocr.original_coordinates(ocr.parse_ocr(tsv))
    return words, {'original': png.name, 'processed': processed.name, 'tsv': tsv_path.name,
                   'preprocessing': command, 'scale': 3, 'offset': [0, 0], 'page_segmentation': 3}


def ingress_flood_command(backend, pairs=3000):
    assert 0 < pairs <= 3000
    if backend == 'x11':
        return ['xdotool', 'key', '--clearmodifiers', '--delay', '0', '--repeat', str(pairs), 'F24']
    assert backend == 'wayland'
    # wtype rejects an explicit zero delay; its default already has no sleep.
    return ['wtype'] + ['-k', 'F24'] * pairs


def observed_x11_press(log, pid):
    # The fixture records actual Runner callbacks, not the injector's success.
    prefix = f'FROST_FAULT_POINTER pid={pid} stage=runner MouseInput '
    return any(line.startswith(prefix) and 'state: Pressed, button: Left' in line
               for line in log.splitlines())


def x11_observed_click(smoke, point, pid, wait_for):
    smoke.run(['xdotool', 'mousemove', '--sync', *map(str, point)])
    smoke.run(['xdotool', 'mousedown', '1'])
    try:
        wait_for(lambda: observed_x11_press((smoke.out / 'fixture.log').read_text(), pid),
                 'same-process real X11 button press', timeout=5)
    finally:
        # Release even when observation fails; never leave the compositor held.
        smoke.run(['xdotool', 'mouseup', '1'])


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
    scope = SCOPE if manifest.get('mechanism', 'tracker') == 'tracker' else 'derived fixture: actual Runner admission under controlled consumer stall and real compositor keys; not natural flood'
    result = {'scope': scope, 'backend': args.backend, 'binary_sha256': digest, 'status': 'fail'}
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
        runner.wait_for(proof.with_suffix('.armed').exists, 'fixture admission armed', timeout=10)
        control.write_text('trigger real admission\n')
        if manifest.get('mechanism') == 'runner':
            flood = smoke.spawn(ingress_flood_command(args.backend), 'ingress-flood')
            result['real_compositor_key_pairs_requested'] = 3000
        runner.wait_for(proof.exists, 'real admission proof', timeout=20)
        admission = json.loads(proof.read_text())
        assert admission['pid'] == app.pid, 'Proof did not originate from the visible application process'
        assert admission['input_overloaded'] is True, admission
        assert 0 < admission['events_broadcast'] <= 65536, admission
        assert admission['natural_winit_flood'] is False, admission
        assert admission['direct_latch_set_or_reset'] is False, admission
        if manifest.get('mechanism') == 'runner':
            assert admission['runner_admission_rejected'] is True, admission
            assert admission['consumer_stall_ms'] < 20000, admission
            assert 0 < admission['events_broadcast'] <= 6000, admission
            assert flood.wait(timeout=10) == 0, 'Real compositor key flood failed'
        result['admission'] = admission
        if manifest.get('mechanism') == 'runner':
            result['consumer_milestones'] = {}
            for label in ['consumer', 'view-rebuilt', 'drawn', 'presented']:
                path = proof.with_suffix('.' + label)
                runner.wait_for(path.exists, 'postfault ' + label, timeout=10)
                pid = int(path.read_text())
                assert pid == app.pid, 'Consumer milestone belongs to another process'
                result['consumer_milestones'][label] = pid
        ongoing_flood = None
        if manifest.get('mechanism') == 'runner':
            if args.backend == 'x11':
                ongoing_flood = smoke.spawn(['xdotool', 'key', '--delay', '2', '--repeat', '20000', 'F24'], 'fault-keyboard-flood')
            else:
                ongoing_flood = smoke.spawn(['wtype'] + ['-s', '2', '-k', 'F24'] * 20000, 'fault-keyboard-flood')
        def fault_key_count():
            pid, count = map(int, proof.with_suffix('.fault-keys').read_text().split())
            assert pid == app.pid, 'Fault input observation belongs to another process'
            return count
        if ongoing_flood is not None:
            runner.wait_for(lambda: proof.with_suffix('.fault-keys').exists(), 'actual postfault native keys', timeout=5)
            flood_count_before = fault_key_count()
        observations = []

        def observe_fault():
            assert app.poll() is None, 'Application exited before fault UI observation'
            name = 'fault-' + str(len(observations))
            smoke.screenshot(name)
            words, transform = capture_fault_ocr(out / (name + '.png'), smoke.run)
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
        if ongoing_flood is not None:
            assert ongoing_flood.poll() is None, 'Keyboard flood ended before testing reserved close path'
            flood_count_after = fault_key_count()
            assert flood_count_after > flood_count_before, 'No observed native keyboard flood progress during fault UI verification'
            result['fault_keyboard_flood_active_at_click'] = True
            result['observed_fault_keyboard_counts'] = [flood_count_before, flood_count_after]
        result['click'] = {'text': BUTTON, 'point': point, 'method': 'native compositor pointer'}
        if args.backend == 'x11':
            if manifest.get('mechanism') == 'runner':
                x11_observed_click(smoke, point, app.pid, runner.wait_for)
                result['x11_same_pid_press_observed_before_release'] = True
            else:
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


def regression_test(output):
    """Replay frozen genuine CI frames through the exact production observer."""
    import shutil
    fixtures = HERE.parent / 'tests/fixtures/native_overload_observation'
    manifest = json.loads((fixtures / 'manifest.json').read_text())
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    results = []
    for name, digest in manifest['frames'].items():
        source = fixtures / name
        assert hashlib.sha256(source.read_bytes()).hexdigest() == digest, name
        target = output / name
        shutil.copy2(source, target)
        def run(command, **kwargs):
            return subprocess.run(command, check=True, timeout=30, **kwargs)
        words, transform = capture_fault_ocr(target, run)
        point = fault_button(words)
        assert point is not None, f'Exact fault warning/button not observed in {name}'
        results.append({'frame': name, 'sha256': digest, 'button_from_ocr': point, 'transform': transform})
    (output / 'result.json').write_text(json.dumps({'status': 'pass', 'scope': 'frozen screenshot observer regression only', 'frames': results}, indent=2))
    print(json.dumps({'status': 'pass', 'frames': len(results), 'scope': 'observer regression only'}))


class ObservationTests(unittest.TestCase):
    def test_press_proof_requires_matching_pid_stage_and_button(self):
        good = 'FROST_FAULT_POINTER pid=42 stage=runner MouseInput { state: Pressed, button: Left }'
        self.assertTrue(observed_x11_press(good, 42))
        for wrong in [good.replace('pid=42', 'pid=43'), good.replace('runner', 'consumer'),
                      good.replace('Pressed', 'Released'), good.replace('Left', 'Right')]:
            self.assertFalse(observed_x11_press(wrong, 42))

    def test_x11_release_follows_observed_press_without_retry(self):
        from unittest.mock import Mock
        smoke = Mock()
        smoke.out = Mock()
        smoke.out.__truediv__ = Mock(return_value=Mock())
        order = []
        smoke.run.side_effect = lambda command: order.append(command[1])
        def seen(probe, description, timeout):
            order.append('observed-press')
            self.assertEqual(timeout, 5)
            return True
        x11_observed_click(smoke, (413, 531), 42, seen)
        self.assertEqual(order, ['mousemove', 'mousedown', 'observed-press', 'mouseup'])

    def test_x11_release_is_guaranteed_when_press_not_delivered(self):
        from unittest.mock import Mock
        smoke = Mock()
        def fail(*args, **kwargs):
            raise TimeoutError('no observed native press')
        with self.assertRaises(TimeoutError):
            x11_observed_click(smoke, (413, 531), 42, fail)
        self.assertEqual([call.args[0] for call in smoke.run.call_args_list], [
            ['xdotool', 'mousemove', '--sync', '413', '531'],
            ['xdotool', 'mousedown', '1'], ['xdotool', 'mouseup', '1']])


    def test_wayland_flood_uses_valid_default_zero_delay(self):
        self.assertEqual(ingress_flood_command('wayland', 2), ['wtype', '-k', 'F24', '-k', 'F24'])
        self.assertNotIn('-d', ingress_flood_command('wayland'))
        self.assertEqual(ingress_flood_command('x11', 2)[-3:], ['--repeat', '2', 'F24'])

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
    elif len(sys.argv) == 3 and sys.argv[1] == '--regression-test':
        regression_test(sys.argv[2])
    else:
        main()
