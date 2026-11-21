#!/usr/bin/env python3
"""Synthetic marker classifications through the native smoke's real predicate."""
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
import unittest

path = Path(__file__).with_name('native-tab-resize-smoke.py')
spec = spec_from_file_location('tab_resize_smoke', path)
smoke = module_from_spec(spec)
spec.loader.exec_module(smoke)


class ResizeFrameEvidence(unittest.TestCase):
    prior = smoke.ready_color(100)
    current = smoke.ready_color(92)

    def evidence(self, colors, columns=92):
        return smoke.resize_frame_evidence(set(colors), 100, columns)

    def test_exact_prior_and_current_witnesses_are_distinct(self):
        self.assertEqual(self.evidence([self.prior]), 'previous-only')
        self.assertEqual(self.evidence([self.current]), 'current-only')

    def test_mixed_cannot_be_either_old_only_or_complete(self):
        self.assertEqual(self.evidence([self.prior, self.current]), 'mixed')

    def test_no_retention_credit_before_pty_resizes(self):
        self.assertEqual(self.evidence([self.prior], 100), 'awaiting-pty-resize')

    def test_no_completion_credit_without_either_marker(self):
        self.assertEqual(self.evidence([]), 'unmarked')

    def test_replay_marker_never_exempt_even_before_resize(self):
        for cols in (100, 92):
            with self.subTest(cols=cols), self.assertRaisesRegex(AssertionError, 'intermediate'):
                self.evidence([smoke.REPLAY_COLOR, self.current], cols)

    def test_colliding_paint_codes_cannot_fake_transition(self):
        with self.assertRaisesRegex(AssertionError, 'collide'):
            self.evidence([self.prior], 280)


if __name__ == '__main__':
    unittest.main()
