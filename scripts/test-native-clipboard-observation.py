#!/usr/bin/env python3
"""Cursor-blink observations never repair, inject, or accept incorrect text."""
import unittest
from unittest.mock import patch

from native_clipboard_observation import observe_exact_word


class ExactObservationTests(unittest.TestCase):
    def test_blink_frame_is_retained_before_exact_frame(self):
        artifacts, observations = [], []
        frames = iter([['FROSTWIDGETI'], ['FROSTWIDGET']])

        def capture(label):
            artifacts.append(label)
            return [{'text': text} for text in next(frames)]

        with patch('native_clipboard_observation.time.monotonic', side_effect=[0, 0, 1]):
            observe_exact_word(capture, 'pasted', 'FROSTWIDGET', observations)
        self.assertEqual(artifacts, ['pasted-00', 'pasted-01'])
        self.assertEqual([item['words'] for item in observations],
                         [['FROSTWIDGETI'], ['FROSTWIDGET']])

    def test_missing_wrong_partial_and_duplicate_tokens_time_out(self):
        for tokens in ([], ['WRONG'], ['FROSTWIDGETI'], ['FROST', 'WIDGET'],
                       ['FROSTWIDGET', 'FROSTWIDGET']):
            with self.subTest(tokens=tokens):
                observations = []
                with patch('native_clipboard_observation.time.monotonic', side_effect=[0, 0, 6]):
                    with self.assertRaises(AssertionError):
                        observe_exact_word(lambda label: [{'text': word} for word in tokens],
                                           'pasted', 'FROSTWIDGET', observations)
                self.assertEqual(observations, [{'artifact': 'pasted-00', 'words': tokens}])


if __name__ == '__main__':
    unittest.main()
