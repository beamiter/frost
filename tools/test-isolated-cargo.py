#!/usr/bin/env python3
"""Regression: standalone Cargo invocations cannot see a host checkout patch."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).resolve().parents[1] / 'scripts/isolated_cargo.py'


class IsolatedCargoTest(unittest.TestCase):
    def test_checkout_config_is_excluded_and_exit_status_and_arguments_are_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            checkout = root / 'checkout'
            (checkout / '.cargo').mkdir(parents=True)
            (checkout / '.cargo/config.toml').write_text('poison checkout-only patch')
            manifest = checkout / 'standalone/Cargo.toml'
            manifest.parent.mkdir()
            manifest.write_text('[workspace]\n')
            lock = manifest.with_name('Cargo.lock')
            lock.write_text('frozen fixture lock\n')
            binary_directory = root / 'bin'
            binary_directory.mkdir()
            cargo = binary_directory / 'cargo'
            cargo.write_text('#!' + sys.executable + '\n' + r'''
import json
from pathlib import Path
import sys
assert not any((p / '.cargo/config.toml').exists() for p in [Path.cwd(), *Path.cwd().parents])
print(json.dumps({'cwd': str(Path.cwd()), 'arguments': sys.argv[1:]}))
raise SystemExit(7)
''')
            cargo.chmod(0o755)
            result = subprocess.run(
                [sys.executable, str(RUNNER), '--manifest-path', 'standalone/Cargo.toml',
                 'test', '--locked', '--', '--ignored', '--test-threads=1'],
                cwd=checkout,
                env={**os.environ, 'PATH': str(binary_directory) + os.pathsep + os.environ['PATH']},
                text=True, capture_output=True,
            )
            self.assertEqual(result.returncode, 7, result.stderr)
            observed = json.loads(result.stdout)
            self.assertEqual(observed['arguments'],
                             ['test', '--manifest-path', str(manifest), '--locked', '--',
                              '--ignored', '--test-threads=1'])
            self.assertFalse(Path(observed['cwd']).exists(), 'private work directory must be cleaned')
            self.assertEqual(lock.read_text(), 'frozen fixture lock\n')
            self.assertEqual(manifest.read_text(), '[workspace]\n')


if __name__ == '__main__':
    unittest.main()
