#!/usr/bin/env python3
"""Independent fixture-reader negatives; no Vim, GUI or third-party modules."""
import ast
from contextlib import ExitStack
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

from native_vim_snapshot import (FixtureSnapshotReader, require_unchanged_quickfix,
                                 validate_geometry, validate_quickfix)


def geometry(sequence=1, columns=115, rows=39):
    return {"sample_seq": sequence, "screen_columns": columns,
            "screen_rows": rows, "line": 1, "column": 1, "topline": 1,
            "windows": 1, "quickfix_size": 0, "leftcol": 0,
            "termguicolors": 0, "pumvisible": 0, "modified": 0, "selected": -1,
            "mode": "n", "cmdline": "", "text": "original", "completion_line": "",
            "quickfix_title": "", "render": {}}


def quickfix(sequence=1, title="Search: kept", text="kept"):
    return {"sample_seq": sequence, "title": title, "idx": 1,
            "items": [{"bufnr": 5, "lnum": 1, "col": 1,
                       "type": "I", "valid": 1, "text": text}]}


class ScriptedPath:
    """Deterministic file-read outcomes; the last outcome persists."""
    def __init__(self, *outcomes):
        self.outcomes = outcomes
        self.reads = 0

    def read_text(self, **_):
        value = self.outcomes[min(self.reads, len(self.outcomes) - 1)]
        self.reads += 1
        if callable(value):
            value = value()
        if isinstance(value, BaseException):
            raise value
        return value if isinstance(value, str) else json.dumps(value)

    def __str__(self):
        return "fixture.json"


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def sleep(self, seconds):
        self.now += seconds


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.clock = FakeClock()
        self.stack.enter_context(patch("native_vim_snapshot.time.monotonic", lambda: self.clock.now))
        self.stack.enter_context(patch("native_vim_snapshot.time.sleep", self.clock.sleep))
        self.process = Mock()
        self.process.poll.return_value = None

    def reader(self, *outcomes, validate=validate_geometry):
        path = ScriptedPath(*outcomes)
        return FixtureSnapshotReader(path, validate), path

    def test_real_file_and_geometry_are_one_complete_record(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            expected = geometry()
            path.write_text(json.dumps(expected), encoding="utf-8")
            sample = FixtureSnapshotReader(path, validate_geometry).wait(self.process, "geometry")
            self.assertEqual((sample["screen_columns"], sample["screen_rows"]), (115, 39))

    def test_missing_and_partial_reads_retry_without_cached_data(self):
        reader, path = self.reader(FileNotFoundError("rename gap"), "", '{"screen_columns":', geometry(2))
        self.assertEqual(reader.wait(self.process, "geometry"), geometry(2))
        self.assertEqual(path.reads, 4)

    def test_missing_file_times_out_with_last_error(self):
        reader, _ = self.reader(FileNotFoundError("rename gap"))
        with self.assertRaisesRegex(RuntimeError, "Timed out: geometry.*FileNotFoundError.*last_raw=None"):
            reader.wait(self.process, "geometry", timeout=0.025)

    def test_partial_json_times_out_with_exact_last_raw(self):
        reader, _ = self.reader('{"screen_columns":')
        with self.assertRaises(RuntimeError) as error:
            reader.wait(self.process, "geometry", timeout=0.025)
        self.assertIn("JSONDecodeError", str(error.exception))
        self.assertIn("last_raw='{\"screen_columns\":'", str(error.exception))

    def test_non_retryable_io_error_propagates(self):
        reader, path = self.reader(PermissionError("denied"), geometry())
        with self.assertRaises(PermissionError):
            reader.wait(self.process, "geometry")
        self.assertEqual(path.reads, 1)

    def test_complete_bad_json_types_fail_without_retry(self):
        for value in ([], None, 1, "true", '"text"', {}):
            with self.subTest(value=value):
                reader, path = self.reader(value, geometry())
                with self.assertRaisesRegex(AssertionError, "Invalid Vim fixture sample"):
                    reader.wait(self.process, "geometry")
                self.assertEqual(path.reads, 1)

    def test_invalid_or_missing_sequences_fail_even_when_stale(self):
        for value in (None, 0, -1, True, 1.5, "2"):
            with self.subTest(sequence=value):
                sample = geometry()
                sample["sample_seq"] = value
                reader, path = self.reader(sample, geometry(4))
                with self.assertRaisesRegex(AssertionError, "invalid sample_seq"):
                    reader.wait(self.process, "geometry", after_sequence=2)
                self.assertEqual(path.reads, 1)

    def test_geometry_field_schema_negatives_fail_without_retry(self):
        invalid = []
        for field in ("screen_columns", "screen_rows"):
            for value in (None, 0, -1, True, "115", 39.0):
                sample = geometry()
                sample[field] = value
                invalid.append(sample)
            sample = geometry()
            del sample[field]
            invalid.append(sample)
        for value in (None, [], "{}", True):
            sample = geometry()
            sample["render"] = value
            invalid.append(sample)
        for sample in invalid:
            with self.subTest(sample=sample):
                reader, path = self.reader(sample, geometry(4))
                with self.assertRaises(AssertionError):
                    reader.wait(self.process, "geometry", after_sequence=2)
                self.assertEqual(path.reads, 1)

    def test_every_required_buffer_field_rejects_missing_or_wrong_type(self):
        for field in geometry():
            for missing in (True, False):
                with self.subTest(field=field, missing=missing):
                    sample = geometry()
                    if missing:
                        del sample[field]
                    else:
                        sample[field] = None
                    reader, path = self.reader(sample, geometry(4))
                    with self.assertRaises(AssertionError):
                        reader.wait(self.process, "buffer state", after_sequence=2)
                    self.assertEqual(path.reads, 1)

    def test_sequence_regression_is_not_treated_as_stale(self):
        reader, path = self.reader(geometry(3), geometry(2), geometry(4))
        self.assertEqual(reader.poll(), geometry(3))
        with self.assertRaisesRegex(AssertionError, "sample_seq moved backwards"):
            reader.wait(self.process, "geometry", after_sequence=3)
        self.assertEqual(path.reads, 2)

    def test_wait_skips_only_legal_old_sequence(self):
        reader, path = self.reader(geometry(3), geometry(3), geometry(4))
        self.assertEqual(reader.wait(self.process, "geometry", after_sequence=3), geometry(4))
        self.assertEqual(path.reads, 3)

    def test_fresh_excludes_initial_old_and_in_flight_records(self):
        reader, path = self.reader(geometry(7), geometry(8), geometry(8), geometry(9, 90, 34))
        sample = reader.fresh(self.process, "geometry after action")
        self.assertEqual((sample["screen_columns"], sample["screen_rows"]), (90, 34))
        self.assertEqual(path.reads, 4)

    def test_fresh_baseline_is_observed_when_called_not_a_previous_read(self):
        reader, path = self.reader(geometry(2), geometry(10), geometry(11), geometry(12))
        self.assertEqual(reader.poll()["sample_seq"], 2)
        self.assertEqual(reader.fresh(self.process, "post-stop geometry")["sample_seq"], 12)
        self.assertEqual(path.reads, 4)

    def test_static_complete_sample_times_out_instead_of_being_fresh(self):
        reader, _ = self.reader(geometry(8))
        with self.assertRaisesRegex(RuntimeError, "Timed out: fresh.*after_sequence=9"):
            reader.fresh(self.process, "fresh", timeout=0.025)

    def test_fresh_shares_one_deadline_across_initial_and_new_reads(self):
        reader, _ = self.reader("", "", "", geometry(1), geometry(2), geometry(3))
        with self.assertRaisesRegex(RuntimeError, "Timed out: fresh"):
            reader.fresh(self.process, "fresh", timeout=0.035)
        self.assertLess(self.clock.now, 0.045)

    def test_complete_record_arriving_after_deadline_is_rejected(self):
        def delayed():
            self.clock.now += 1
            return geometry(1)
        reader, _ = self.reader(delayed)
        with self.assertRaisesRegex(RuntimeError, "Timed out"):
            reader.wait(self.process, "late", timeout=0.5)

    def test_process_exit_is_not_hidden_by_a_valid_sample(self):
        self.process.poll.return_value = 9
        reader, path = self.reader(geometry())
        with self.assertRaisesRegex(RuntimeError, "application exited 9"):
            reader.wait(self.process, "geometry")
        self.assertEqual(path.reads, 0)

    def test_quickfix_schema_negatives_fail_without_retry(self):
        invalid = []
        for field, value in (("title", None), ("idx", True), ("idx", -1), ("items", {})):
            sample = quickfix()
            sample[field] = value
            invalid.append(sample)
        for field in ("title", "idx", "items"):
            sample = quickfix()
            del sample[field]
            invalid.append(sample)
        for field, value in (("bufnr", "5"), ("lnum", True), ("col", -1),
                             ("text", []), ("type", None), ("valid", True), ("valid", 2)):
            sample = quickfix()
            sample["items"][0][field] = value
            invalid.append(sample)
        sample = quickfix()
        sample["items"] = ["not an item"]
        invalid.append(sample)
        for sample in invalid:
            with self.subTest(sample=sample):
                reader, path = self.reader(sample, quickfix(4), validate=validate_quickfix)
                with self.assertRaises(AssertionError):
                    reader.wait(self.process, "quickfix", after_sequence=2)
                self.assertEqual(path.reads, 1)

    def test_cancelled_quickfix_torn_reads_keep_strict_original_rows(self):
        expected = quickfix()
        reader, _ = self.reader("", quickfix(8), '{"items":', quickfix(9), quickfix(10),
                                validate=validate_quickfix)
        sample = reader.fresh(self.process, "cancelled quickfix")
        require_unchanged_quickfix(sample, expected["title"], expected["items"])

    def test_complete_wrong_quickfix_is_not_retried_until_equal(self):
        expected = quickfix()
        for field in ("title", "items"):
            with self.subTest(field=field):
                wrong = quickfix(10)
                wrong[field] = "Search: replaced" if field == "title" else []
                reader, path = self.reader(quickfix(8), quickfix(9), wrong, quickfix(11),
                                           validate=validate_quickfix)
                sample = reader.fresh(self.process, "cancelled quickfix")
                with self.assertRaisesRegex(AssertionError, "Cancelled search replaced"):
                    require_unchanged_quickfix(sample, expected["title"], expected["items"])
                self.assertEqual(path.reads, 3)

    def test_row_content_changes_cannot_pass_with_same_title_and_count(self):
        expected = quickfix()
        actual = copy.deepcopy(expected)
        actual["items"][0]["text"] = "wrong result"
        with self.assertRaisesRegex(AssertionError, "Cancelled search replaced"):
            require_unchanged_quickfix(actual, expected["title"], expected["items"])

    def test_actual_geometry_observer_reads_once_and_returns_that_record(self):
        # Execute the actual nested probe without importing Pillow or starting
        # the GUI. A second read would see the fixture's next missing-file gap.
        source = Path(__file__).with_name("native-vim9-buffer-smoke.py").read_text()
        tree = ast.parse(source)
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        function = next(node for node in main.body if isinstance(node, ast.FunctionDef)
                        and node.name == "geometry_observation")
        sample = geometry(9, 90, 34)
        state = Mock(side_effect=[sample, {}])
        namespace = {"state": state}
        exec(compile(ast.Module(body=[function], type_ignores=[]), str(__file__), "exec"), namespace)
        found = namespace["geometry_observation"](lambda value: value["screen_columns"] < 115)
        self.assertIs(found, sample)
        self.assertEqual(state.call_count, 1)
        self.assertIsNone(namespace["geometry_observation"](lambda value: True))


if __name__ == "__main__":
    unittest.main(verbosity=2)
