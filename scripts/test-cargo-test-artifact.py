#!/usr/bin/env python3
"""Regression tests for Cargo-reported provider artifact discovery."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('record_artifact', Path(__file__).with_name('record-cargo-test-artifact.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.manifest = self.root / 'smithay-clipboard/Cargo.toml'
        self.manifest.parent.mkdir()
        self.manifest.write_text('[package]\nname="smithay-clipboard"\n')
        self.binary = self.root / 'target/debug/build/smithay-clipboard/hash/out/smithay_clipboard-hash'
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b'reviewed executable')
        self.binary.chmod(0o755)
        self.artifact = dict(reason='compiler-artifact', profile={'test': True},
                             target={'name': 'smithay_clipboard', 'kind': ['lib']},
                             manifest_path=str(self.manifest), executable=str(self.binary))
        self.finished = dict(reason='build-finished', success=True)

    def check(self, events):
        return module.record('\n'.join(map(json.dumps, events)), self.manifest, 'smithay_clipboard')

    def test_new_layout_without_deps(self):
        self.assertFalse((self.root / 'target/debug/deps').exists())
        expected = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        self.assertEqual(self.check([self.artifact, self.finished]), f'{expected}  {self.binary}\n')

    def test_legacy_layout_and_cached_artifact(self):
        binary = self.root / 'target/debug/deps/smithay_clipboard-old'
        binary.parent.mkdir(parents=True)
        self.binary.rename(binary)
        self.artifact.update(executable=str(binary), fresh=True)
        self.assertIn(str(binary), self.check([self.artifact, self.finished]))

    def test_stale_files_do_not_supply_missing_artifacts(self):
        with self.assertRaises(ValueError):
            self.check([self.finished])

    def test_failed_or_unfinished_build(self):
        for end in [[], [dict(reason='build-finished', success=False)]]:
            with self.subTest(end=end), self.assertRaises(ValueError):
                self.check([self.artifact, *end])

    def test_wrong_identity_or_non_test_target(self):
        for change in [dict(manifest_path=str(self.root/'other.toml')),
                       dict(profile={'test': False}), dict(executable=None),
                       dict(target={'name': 'other', 'kind': ['lib']}),
                       dict(target={'name': 'smithay_clipboard', 'kind': ['bin']})]:
            wrong = copy.deepcopy(self.artifact)
            wrong.update(change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.check([wrong, self.finished])

    def test_ambiguous_executables(self):
        second = self.root/'second'
        second.write_bytes(b'another'); second.chmod(0o755)
        with self.assertRaises(ValueError):
            self.check([self.artifact, dict(self.artifact, executable=str(second)), self.finished])

    def test_missing_and_non_executable_file(self):
        self.binary.chmod(0o644)
        with self.assertRaises(ValueError):
            self.check([self.artifact, self.finished])
        self.binary.unlink()
        with self.assertRaises(FileNotFoundError):
            self.check([self.artifact, self.finished])

    def test_malformed_json(self):
        with self.assertRaises(json.JSONDecodeError):
            module.record('not JSON', self.manifest, 'smithay_clipboard')


if __name__ == '__main__':
    unittest.main()
