#!/usr/bin/env python3
"""Cleanup fault injection only; not compositor or application acceptance."""
import importlib.util
import io
from pathlib import Path
import subprocess
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('smoke', Path(__file__).with_name('native-desktop-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class CleanupTests(unittest.TestCase):
    def fixture(self, process):
        fixture = object.__new__(smoke.Smoke)
        fixture.processes = [Mock(), process]
        fixture.processes[0].poll.return_value = 0
        fixture.logs = [Mock()]
        fixture._runtime = Mock()
        return fixture

    def test_kill_wait_remains_bounded_and_releases_resources(self):
        process = Mock()
        process.poll.return_value = None
        process.wait.side_effect = subprocess.TimeoutExpired('child', 5)
        fixture = self.fixture(process)
        with self.assertRaises(RuntimeError):
            fixture.close()
        self.assertEqual(process.wait.call_args_list, [unittest.mock.call(timeout=5)] * 2)
        fixture.processes[0].poll.assert_called_once()
        fixture.logs[0].close.assert_called_once()
        fixture._runtime.cleanup.assert_called_once()

    def test_cleanup_failure_preserves_original_acceptance_error(self):
        process = Mock()
        process.poll.side_effect = OSError('injected')
        fixture = self.fixture(process)
        with patch.object(smoke.sys, 'stderr', io.StringIO()) as stderr:
            with self.assertRaisesRegex(ValueError, 'acceptance failed'):
                try:
                    raise ValueError('acceptance failed')
                finally:
                    fixture.close()
            self.assertIn('cleanup failed', stderr.getvalue())
        fixture._runtime.cleanup.assert_called_once()

    def test_normal_cleanup_terminates_without_kill(self):
        process = Mock()
        process.poll.return_value = None
        fixture = self.fixture(process)
        fixture.close()
        process.terminate.assert_called_once()
        process.wait.assert_called_once_with(timeout=5)
        process.kill.assert_not_called()
        fixture._runtime.cleanup.assert_called_once()


if __name__ == '__main__':
    unittest.main()
