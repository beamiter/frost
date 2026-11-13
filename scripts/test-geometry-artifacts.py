#!/usr/bin/env python3
"""Fail-closed dependency identity checks for old/new Cargo artifact layouts."""
import json
from pathlib import Path
import tempfile
import unittest
import native_scale_geometry as geometry


class ArtifactLayouts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.profile = Path(self.temp.name) / 'release'
        self.deps = self.profile / 'deps'

    def fixture(self, modern):
        base = self.profile / ('build/frost/app/fingerprint' if modern else '.fingerprint/frost-app')
        base.mkdir(parents=True)
        (base/'bin-frost.json').write_text(json.dumps({'rustc': 9, 'deps': [[0, 'iced', 0, 42], [0, 'cosmic_text', 0, 43]]}))
        self.paths = []
        for name, identity in [('iced', 42), ('cosmic_text', 43)]:
            package = name.replace('_', '-')
            root = self.profile / 'build' / package / 'abc'
            fingerprint = root/'fingerprint' if modern else self.profile/'.fingerprint'/(package+'-abc')
            library = root/'out'/('lib'+name+'-abc.rlib') if modern else self.deps/('lib'+name+'-abc.rlib')
            fingerprint.mkdir(parents=True)
            library.parent.mkdir(parents=True, exist_ok=True)
            library.write_bytes(b'fixture')
            metadata = fingerprint/('lib-'+name+'.json')
            metadata.write_text(json.dumps({'rustc': 9}))
            metadata.with_suffix('').write_text(identity.to_bytes(8, 'little').hex())
            self.paths.append((library, metadata))

    def test_both_layouts(self):
        for modern in [False, True]:
            with self.subTest(modern=modern):
                # Use distinct profile roots for each fixture.
                self.profile = Path(self.temp.name) / str(modern) / 'release'
                self.deps = self.profile/'deps'
                self.fixture(modern)
                _, libraries, search = geometry.locked_probe_dependencies(self.deps)
                self.assertEqual(set(libraries), {'iced', 'cosmic_text'})
                self.assertTrue(all(p.parent in search for p in libraries.values()))

    def test_transitive_proc_macro_search_directory(self):
        self.fixture(True)
        output = self.profile/'build/derive/abc/out'
        output.mkdir(parents=True)
        (output/'libderive-abc.so').write_bytes(b'proc macro')
        _, _, search = geometry.locked_probe_dependencies(self.deps)
        self.assertIn(output, search)

    def test_compiler_mismatch(self):
        self.fixture(True)
        self.paths[0][1].write_text(json.dumps({'rustc': 10}))
        with self.assertRaises(AssertionError): geometry.locked_probe_dependencies(self.deps)

    def test_dependency_identity_mismatch(self):
        self.fixture(True)
        self.paths[0][1].with_suffix('').write_text((1).to_bytes(8, 'little').hex())
        with self.assertRaises(AssertionError): geometry.locked_probe_dependencies(self.deps)

    def test_missing_rlib(self):
        self.fixture(True)
        self.paths[0][0].unlink()
        with self.assertRaises(AssertionError): geometry.locked_probe_dependencies(self.deps)

    def test_ambiguous_matching_rlib(self):
        self.fixture(True)
        import shutil
        original = self.profile/'build/iced/abc'
        duplicate = original.with_name('other')
        shutil.copytree(original, duplicate)
        with self.assertRaises(AssertionError): geometry.locked_probe_dependencies(self.deps)


if __name__ == '__main__': unittest.main()
