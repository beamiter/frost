#!/usr/bin/env python3
"""Exercise the real close driver without a compositor; not native coverage."""
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('native-desktop-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class Pointer:
    pid = 123
    def __init__(self):
        self.stdin = io.BytesIO()
    def poll(self):
        return None


class App:
    pid = 99999999
    returncode = None
    def __init__(self, timeout):
        self.timeout = timeout
        self.waits = []
    def poll(self):
        return self.returncode
    def wait(self, timeout):
        self.waits.append(timeout)
        if self.timeout:
            raise subprocess.TimeoutExpired('frost', timeout)
        self.returncode = 0


class CloseEvidenceTests(unittest.TestCase):
    def exercise(self, timeout):
        with tempfile.TemporaryDirectory() as directory:
            fixture = object.__new__(smoke.Smoke)
            fixture.backend = 'wayland'
            fixture.out = Path(directory)
            fixture.env = {'FROST_SMOKE_POINTER': 'synthetic-test-pointer'}
            fixture.logs = []
            fixture.processes = []
            fixture.run = lambda *args: None
            fixture.observe_frame = lambda *args, **kw: ('warning', [])
            fixture.close_button = lambda rows: (359, 525)
            fixture.pointer_reply = lambda *args: None
            pointer = Pointer()
            app = App(timeout)
            with patch.object(smoke.subprocess, 'Popen', return_value=pointer):
                if timeout:
                    with self.assertRaises(subprocess.TimeoutExpired):
                        fixture.close_app(app)
                else:
                    fixture.close_app(app)
            self.assertEqual(app.waits, [20])
            self.assertEqual(pointer.stdin.getvalue(), b'click 359 525 1280 900\n')
            evidence = json.loads((fixture.out / 'close-attempt-1.json').read_text())
            stages = [event['stage'] for event in evidence['events']]
            self.assertLessEqual(len(stages), 32)
            self.assertEqual(stages[-1], 'failed' if timeout else 'process_exited')
            ack = next(e for e in evidence['events'] if e['stage'] == 'compositor_ack')
            self.assertFalse(ack['application_delivery_proven'])
            self.assertEqual(evidence['failure'], 'TimeoutExpired' if timeout else None)
            for log in fixture.logs:
                log.close()
    def test_success_preserves_single_click_and_deadline(self):
        self.exercise(False)
    def test_timeout_propagates_and_preserves_evidence(self):
        self.exercise(True)
    def test_failed_evidence_write_preserves_original_timeout(self):
        fixture = object.__new__(smoke.Smoke)
        fixture.out = Path('/unused-test-directory')
        def fail(app, trace):
            raise subprocess.TimeoutExpired('frost', 20)
        fixture._close_app = fail
        with patch.object(Path, 'write_text', side_effect=OSError), patch.object(smoke.sys, 'stderr', io.StringIO()):
            with self.assertRaises(subprocess.TimeoutExpired):
                fixture.close_app(App(True))

    def test_evidence_attempt_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = object.__new__(smoke.Smoke)
            fixture.out = Path(directory)
            fixture._close_attempts = 16
            fixture._close_app = lambda app, trace: None
            fixture.close_app(App(False))
            self.assertEqual(list(fixture.out.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
