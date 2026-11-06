"""Read independent Vim fixture samples without confusing torn reads with failures.

Only missing files and incomplete JSON are retryable. A complete record must
have the expected schema, even when it is older than the requested sequence.
Content assertions belong to callers, after acquisition, never in this reader.
"""
from __future__ import annotations

import json
from pathlib import Path
import time


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def validate_geometry(sample: dict) -> None:
    """Validate all always-present native-buffer fixture fields."""
    for name in ("screen_columns", "screen_rows", "line", "column", "topline", "windows"):
        _require(type(sample.get(name)) is int and sample[name] > 0,
                 "invalid positive integer " + name)
    for name in ("quickfix_size", "leftcol"):
        _require(type(sample.get(name)) is int and sample[name] >= 0,
                 "invalid nonnegative integer " + name)
    for name in ("termguicolors", "pumvisible", "modified"):
        _require(type(sample.get(name)) is int and sample[name] in (0, 1),
                 "invalid flag " + name)
    _require(type(sample.get("selected")) is int and sample["selected"] >= -1,
             "invalid completion selection")
    for name in ("mode", "cmdline", "text", "completion_line", "quickfix_title"):
        _require(isinstance(sample.get(name), str), "invalid string " + name)
    _require(isinstance(sample.get("render"), dict), "invalid render object")


def validate_quickfix(sample: dict) -> None:
    _require(isinstance(sample.get("title"), str), "invalid quickfix title")
    _require(type(sample.get("idx")) is int and sample["idx"] >= 0,
             "invalid quickfix index")
    _require(isinstance(sample.get("items"), list), "invalid quickfix items")
    for item in sample["items"]:
        _require(isinstance(item, dict), "invalid quickfix item object")
        for name in ("bufnr", "lnum", "col"):
            _require(type(item.get(name)) is int and item[name] >= 0,
                     "invalid quickfix item " + name)
        for name in ("text", "type"):
            _require(isinstance(item.get(name), str), "invalid quickfix item " + name)
        _require(type(item.get("valid")) is int and item["valid"] in (0, 1),
                 "invalid quickfix item valid flag")


def require_unchanged_quickfix(sample, title, items) -> None:
    """Compare once; a complete wrong list must never trigger another read."""
    _require(sample["title"] == title and sample["items"] == items,
             "Cancelled search replaced the previous quickfix list: " + repr(sample))


class FixtureSnapshotReader:
    def __init__(self, path: Path, validate):
        self.path = path
        self.validate = validate
        self.last_raw = None
        self.last_error = "not read"
        self.last_sequence = 0

    def diagnostic(self) -> str:
        return (f"{self.path}: last_error={self.last_error!r}; "
                f"last_sequence={self.last_sequence}; last_raw={self.last_raw!r}")

    def poll(self):
        try:
            self.last_raw = self.path.read_text(encoding="utf-8")
        except FileNotFoundError as error:
            self.last_raw = None
            self.last_error = repr(error)
            return None
        try:
            sample = json.loads(self.last_raw)
        except json.JSONDecodeError as error:
            self.last_error = repr(error)
            return None
        try:
            _require(isinstance(sample, dict), "expected a JSON object")
            sequence = sample.get("sample_seq")
            _require(type(sequence) is int and sequence > 0, "invalid sample_seq")
            _require(sequence >= self.last_sequence, "sample_seq moved backwards")
            self.validate(sample)
        except AssertionError as error:
            self.last_error = str(error)
            raise AssertionError("Invalid Vim fixture sample: " + self.diagnostic()) from error
        self.last_sequence = sequence
        self.last_error = None
        return sample

    def _wait(self, process, label, deadline, after_sequence):
        while True:
            code = process.poll()
            if code is not None:
                raise RuntimeError(f"{label}: application exited {code}; {self.diagnostic()}")
            if time.monotonic() >= deadline:
                raise RuntimeError(f"Timed out: {label}; after_sequence={after_sequence}; "
                                   + self.diagnostic())
            sample = self.poll()
            if time.monotonic() >= deadline:
                raise RuntimeError(f"Timed out: {label}; after_sequence={after_sequence}; "
                                   + self.diagnostic())
            if sample is not None and sample["sample_seq"] > after_sequence:
                return sample
            time.sleep(0.01)

    def wait(self, process, label, *, after_sequence=0, timeout=5.0):
        """Return one complete sample newer than a known sequence, or fail."""
        return self._wait(process, label, time.monotonic() + timeout, after_sequence)

    def fresh(self, process, label, *, timeout=5.0):
        """Require sampling after this call, within one shared time budget.

        Vim samples serially. The first read might see the old file while the
        next sample is already being written. Requiring two sequence advances
        excludes that in-flight record as well as an unchanged stale file.
        """
        deadline = time.monotonic() + timeout
        baseline = self._wait(process, label, deadline, 0)
        return self._wait(process, label, deadline, baseline["sample_seq"] + 1)
